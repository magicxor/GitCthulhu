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

/// The nearest file row after (`forward`) or before `from`, skipping directory rows; with no `from` the first
/// (`forward`) or the last file row. `None` when there is no such row (the end of `rows`).
fn adjacent_file_row(rows: &[TreeRow], from: Option<usize>, forward: bool) -> Option<usize> {
    let is_file = |row: &usize| matches!(rows[*row], TreeRow::File { .. });
    match (from, forward) {
        (Some(from), true) => (from + 1..rows.len()).find(is_file),
        (Some(from), false) => (0..from.min(rows.len())).rev().find(is_file),
        (None, true) => (0..rows.len()).find(is_file),
        (None, false) => (0..rows.len()).rev().find(is_file),
    }
}

/// The file row that Up / Down move to. `lists` are the rows of the lists from top to bottom; their file rows are
/// walked as one cyclic sequence: directory rows and lists without files are skipped, the first file follows the
/// last one and the other way round. `from` is the `(list, row)` of the current file; without it the walk starts
/// at the first (`forward`) or the last file. `None` when there is no other file row to move to.
pub fn cyclic_file_row(
    lists: &[&[TreeRow]],
    from: Option<(usize, usize)>,
    forward: bool,
) -> Option<(usize, usize)> {
    let count = lists.len();
    // Where the walk enters a list: its first file going down, its last one going up.
    let enter = |list: usize| adjacent_file_row(lists[list], None, forward).map(|row| (list, row));
    let Some((list, row)) = from else {
        return if forward {
            (0..count).find_map(enter)
        } else {
            (0..count).rev().find_map(enter)
        };
    };
    adjacent_file_row(lists.get(list)?, Some(row), forward)
        .map(|row| (list, row))
        .or_else(|| {
            // The other lists in walking order; the last step re-enters this list at its far end.
            (1..=count)
                .map(|step| {
                    if forward {
                        (list + step) % count
                    } else {
                        (list + count - step) % count
                    }
                })
                .find_map(enter)
        })
        .filter(|target| Some(*target) != from)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    const NO_ROWS: &[TreeRow] = &[];

    #[test]
    fn adjacent_file_row_skips_directories_and_stops_at_the_ends() {
        // a/, b/, one.txt, two.txt, top.txt
        let rows = flatten_tree(&["a/b/one.txt", "a/two.txt", "top.txt"], &HashSet::new());
        assert_eq!(adjacent_file_row(&rows, None, true), Some(2));
        assert_eq!(adjacent_file_row(&rows, None, false), Some(4));
        assert_eq!(adjacent_file_row(&rows, Some(2), true), Some(3));
        assert_eq!(adjacent_file_row(&rows, Some(3), false), Some(2));
        assert_eq!(
            adjacent_file_row(&rows, Some(2), false),
            None,
            "only directories above"
        );
        assert_eq!(adjacent_file_row(&rows, Some(4), true), None);
        assert_eq!(adjacent_file_row(&rows, Some(0), true), Some(2));
        assert_eq!(adjacent_file_row(NO_ROWS, None, true), None);
    }

    #[test]
    fn cyclic_file_row_wraps_within_one_list() {
        // a/, b/, one.txt, two.txt, top.txt
        let rows = flatten_tree(&["a/b/one.txt", "a/two.txt", "top.txt"], &HashSet::new());
        let lists = [rows.as_slice()];
        assert_eq!(cyclic_file_row(&lists, Some((0, 2)), true), Some((0, 3)));
        assert_eq!(cyclic_file_row(&lists, Some((0, 3)), false), Some((0, 2)));
        assert_eq!(
            cyclic_file_row(&lists, Some((0, 4)), true),
            Some((0, 2)),
            "past the last file: the first file, below the directory rows"
        );
        assert_eq!(
            cyclic_file_row(&lists, Some((0, 2)), false),
            Some((0, 4)),
            "above the first file: the last one"
        );
        assert_eq!(
            cyclic_file_row(&lists, None, true),
            Some((0, 2)),
            "no current file: Down starts at the first one"
        );
        assert_eq!(
            cyclic_file_row(&lists, None, false),
            Some((0, 4)),
            "and Up at the last one"
        );
    }

    #[test]
    fn cyclic_file_row_crosses_lists_and_skips_empty_ones() {
        let top = flatten_list(&["a", "b"]);
        let bottom = flatten_list(&["c", "d"]);
        let lists = [top.as_slice(), NO_ROWS, bottom.as_slice()];
        assert_eq!(
            cyclic_file_row(&lists, Some((0, 1)), true),
            Some((2, 0)),
            "past the end of a list: the first file of the next list with files"
        );
        assert_eq!(
            cyclic_file_row(&lists, Some((2, 1)), true),
            Some((0, 0)),
            "past the last list: the first one"
        );
        assert_eq!(
            cyclic_file_row(&lists, Some((2, 0)), false),
            Some((0, 1)),
            "above the start of a list: the last file of the previous list with files"
        );
        assert_eq!(
            cyclic_file_row(&lists, Some((0, 0)), false),
            Some((2, 1)),
            "above the first list: the last one"
        );
        let lists = [NO_ROWS, bottom.as_slice(), NO_ROWS];
        assert_eq!(cyclic_file_row(&lists, None, true), Some((1, 0)));
        assert_eq!(cyclic_file_row(&lists, None, false), Some((1, 1)));
        assert_eq!(
            cyclic_file_row(&lists, Some((1, 1)), true),
            Some((1, 0)),
            "the only list with files wraps onto itself"
        );
    }

    #[test]
    fn cyclic_file_row_has_nowhere_to_go_without_another_file() {
        let only = flatten_list(&["a"]);
        let lists = [only.as_slice(), NO_ROWS];
        assert_eq!(
            cyclic_file_row(&lists, Some((0, 0)), true),
            None,
            "the only file stays the current one"
        );
        assert_eq!(cyclic_file_row(&lists, Some((0, 0)), false), None);
        assert_eq!(cyclic_file_row(&[NO_ROWS, NO_ROWS], None, true), None);
        assert_eq!(cyclic_file_row(&[], None, false), None);
        assert_eq!(
            cyclic_file_row(&lists, Some((2, 0)), true),
            None,
            "a position outside the lists"
        );
    }

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
