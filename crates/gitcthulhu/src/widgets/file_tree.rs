//! Flattening of file paths into list or tree rows (Gitnuro 08 §4.4, `ui/tree_files/Tree.kt`).
//!
//! Tree rules: entries are sorted by path component-wise (byte order per component), every directory level is
//! its own row (single-child directories are not compacted), collapsed directories hide all descendants, and all
//! directories are expanded by default (the state is the set of collapsed directory paths, per pane, not
//! persisted). Paths stay byte strings; names are converted at the UI boundary only.

use std::cmp::Ordering;
use std::collections::HashSet;

/// One row of a flattened list or tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    /// A directory row. `path` is the directory's path (the key of the collapsed set).
    Dir {
        path: Vec<u8>,
        name: Vec<u8>,
        depth: u32,
        expanded: bool,
    },
    /// A file row; `index` points into the slice that was flattened.
    File {
        index: usize,
        name: Vec<u8>,
        depth: u32,
    },
}

fn components(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    path.split(|b| *b == b'/')
}

fn compare_paths(a: &[u8], b: &[u8]) -> Ordering {
    components(a).cmp(components(b))
}

/// List mode: one depth-0 file row per path, in the given order.
pub fn flatten_list<P: AsRef<[u8]>>(paths: &[P]) -> Vec<TreeRow> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| TreeRow::File {
            index,
            name: path.as_ref().to_vec(),
            depth: 0,
        })
        .collect()
}

/// Tree mode: sorted rows with directory rows, honouring `collapsed` (directory paths).
pub fn flatten_tree<P: AsRef<[u8]>>(paths: &[P], collapsed: &HashSet<Vec<u8>>) -> Vec<TreeRow> {
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|&a, &b| compare_paths(paths[a].as_ref(), paths[b].as_ref()));

    let mut rows = Vec::new();
    // Directory components already emitted for the previous file.
    let mut open_dirs: Vec<Vec<u8>> = Vec::new();
    for index in order {
        let path = paths[index].as_ref();
        let parts: Vec<&[u8]> = components(path).collect();
        let (name, dirs) = match parts.split_last() {
            Some((name, dirs)) => (*name, dirs),
            None => continue,
        };
        // Keep the shared prefix of directories, emit the new ones.
        let common = open_dirs
            .iter()
            .zip(dirs)
            .take_while(|(open, dir)| open.as_slice() == **dir)
            .count();
        open_dirs.truncate(common);
        let mut hidden = false;
        let mut dir_path: Vec<u8> = Vec::new();
        for (depth, dir) in dirs.iter().enumerate() {
            if depth > 0 {
                dir_path.push(b'/');
            }
            dir_path.extend_from_slice(dir);
            if depth >= common {
                if !hidden {
                    rows.push(TreeRow::Dir {
                        path: dir_path.clone(),
                        name: dir.to_vec(),
                        depth: depth as u32,
                        expanded: !collapsed.contains(&dir_path),
                    });
                }
                open_dirs.push(dir.to_vec());
            }
            if collapsed.contains(&dir_path) {
                hidden = true;
            }
        }
        if !hidden {
            rows.push(TreeRow::File {
                index,
                name: name.to_vec(),
                depth: dirs.len() as u32,
            });
        }
    }
    rows
}

/// The indices of the file rows in display order (directories are excluded, hidden rows are absent): the
/// `visible_order` that Shift-click range selection needs (08 §4.5).
pub fn visible_file_order(rows: &[TreeRow]) -> Vec<usize> {
    rows.iter()
        .filter_map(|row| match row {
            TreeRow::File { index, .. } => Some(*index),
            TreeRow::Dir { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn describe(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Dir {
                    name,
                    depth,
                    expanded,
                    ..
                } => format!(
                    "{}{}/ {}",
                    "  ".repeat(*depth as usize),
                    String::from_utf8_lossy(name),
                    if *expanded { "-" } else { "+" }
                ),
                TreeRow::File { name, depth, .. } => {
                    format!(
                        "{}{}",
                        "  ".repeat(*depth as usize),
                        String::from_utf8_lossy(name)
                    )
                }
            })
            .collect()
    }

    #[test]
    fn tree_has_a_row_per_directory_level_without_compaction() {
        let paths = ["src/a/b/c.rs", "README.md", "src/lib.rs", "src/a/x.rs"];
        let rows = flatten_tree(&paths, &HashSet::new());
        assert_eq!(
            describe(&rows),
            [
                "README.md",
                "src/ -",
                "  a/ -",
                "    b/ -",
                "      c.rs",
                "    x.rs",
                "  lib.rs"
            ],
            "sorted component-wise, one row per level"
        );
    }

    #[test]
    fn collapsed_directories_hide_all_descendants() {
        let paths = ["src/a/b/c.rs", "src/a/x.rs", "src/lib.rs", "z.txt"];
        let collapsed: HashSet<Vec<u8>> = [b"src/a".to_vec()].into_iter().collect();
        let rows = flatten_tree(&paths, &collapsed);
        assert_eq!(
            describe(&rows),
            ["src/ -", "  a/ +", "  lib.rs", "z.txt"],
            "everything below the collapsed directory disappears"
        );
        assert_eq!(
            visible_file_order(&rows),
            [2, 3],
            "range selection only sees visible files"
        );
    }

    #[test]
    fn list_mode_keeps_the_given_order() {
        let rows = flatten_list(&["b.txt", "a.txt"]);
        assert_eq!(
            visible_file_order(&rows),
            [0, 1],
            "list order is the input order"
        );
    }

    #[test]
    fn non_utf8_paths_are_preserved() {
        let paths: [&[u8]; 1] = [b"dir/\xff\xfe.txt"];
        let rows = flatten_tree(&paths, &HashSet::new());
        assert_eq!(
            rows[1],
            TreeRow::File {
                index: 0,
                name: b"\xff\xfe.txt".to_vec(),
                depth: 1
            },
            "bytes are not converted"
        );
    }
}
