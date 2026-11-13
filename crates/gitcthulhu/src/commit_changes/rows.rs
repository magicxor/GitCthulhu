//! Rows of the changed-files list (08 §5.2): the case-insensitive path filter and the list or tree flattening of
//! the files of one commit. Pure functions; the view caches the result and rebuilds it only when the files, the
//! filter, the list/tree mode or the collapsed directories change.

use std::collections::HashSet;

use crate::backend::{ByteSlice as _, CommitFile, ObjectId};
use crate::widgets::file_tree::{TreeRow, flatten_list, flatten_tree};

/// The flattened rows of the file list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    /// The list/tree rows. `TreeRow::File::index` points into [`Rows::visible`].
    pub rows: Vec<TreeRow>,
    /// The files that passed the filter, as indices into the commit's file list.
    pub visible: Vec<usize>,
}

impl Rows {
    /// The number of rows (files and directories).
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there is nothing to show.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The index into the commit's file list of the file row `row`, or `None` for a directory row.
    pub fn file_index(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            TreeRow::File { index, .. } => self.visible.get(*index).copied(),
            TreeRow::Dir { .. } => None,
        }
    }
}

/// Whether `path` contains `needle_lower` (already lower-cased), ignoring case (08 §4.3: case-insensitive
/// substring on the full relative path).
pub fn matches_filter(path: &[u8], needle_lower: &str) -> bool {
    needle_lower.is_empty() || path.to_str_lossy().to_lowercase().contains(needle_lower)
}

/// Filters `files` by `filter` and flattens them as a list (given order) or a tree (sorted, one row per
/// directory level, `collapsed` directories hide their descendants).
pub fn build(files: &[CommitFile], filter: &str, tree: bool, collapsed: &HashSet<Vec<u8>>) -> Rows {
    let needle = filter.to_lowercase();
    let visible: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| matches_filter(file.path.as_slice(), &needle))
        .map(|(ix, _)| ix)
        .collect();
    let paths: Vec<&[u8]> = visible
        .iter()
        .map(|&ix| files[ix].path.as_slice())
        .collect();
    let rows = if tree {
        flatten_tree(&paths, collapsed)
    } else {
        flatten_list(&paths)
    };
    Rows { rows, visible }
}

/// The revision a committed file is shown at by Blame (09 §4.8): the commit the file's diff belongs to (for the
/// untracked files of a stash that is the stash's untracked-files commit), or the first parent when the commit
/// deleted the file (the file does not exist in the commit itself).
pub fn blame_revision(
    file: &CommitFile,
    selected: ObjectId,
    first_parent: Option<ObjectId>,
) -> ObjectId {
    if file.kind == crate::backend::ChangeKind::Deleted
        && let Some(parent) = first_parent
    {
        return parent;
    }
    match &file.target {
        crate::backend::DiffTarget::Commit { commit, .. } => *commit,
        _ => selected,
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;
    use crate::backend::{Against, ChangeKind, DiffTarget, RelaPath};

    fn file(path: &str, kind: ChangeKind) -> CommitFile {
        let commit = ObjectId::from_hex(&[b'0'; 40]).expect("a null id");
        CommitFile {
            path: RelaPath::from(path),
            previous_path: None,
            kind,
            old: None,
            new: None,
            target: DiffTarget::Commit {
                commit,
                against: Against::FirstParent,
                path: RelaPath::from(path),
                previous_path: None,
            },
            lines_added: None,
            lines_removed: None,
        }
    }

    #[test]
    fn filter_is_case_insensitive_substring_of_the_full_path() {
        let files = vec![
            file("src/Main.rs", ChangeKind::Modified),
            file("docs/readme.md", ChangeKind::Added),
            file("src/lib.rs", ChangeKind::Modified),
        ];
        let rows = build(&files, "SRC/", false, &HashSet::new());
        assert_eq!(rows.visible, vec![0, 2]);
        assert_eq!(rows.len(), 2);
        let rows = build(&files, "main", false, &HashSet::new());
        assert_eq!(rows.visible, vec![0]);
        assert!(build(&files, "zzz", false, &HashSet::new()).is_empty());
        assert_eq!(build(&files, "", false, &HashSet::new()).len(), 3);
    }

    #[test]
    fn tree_mode_adds_directory_rows_and_honours_collapsed() {
        let files = vec![
            file("a/b/one.txt", ChangeKind::Added),
            file("a/two.txt", ChangeKind::Modified),
            file("top.txt", ChangeKind::Deleted),
        ];
        let open = build(&files, "", true, &HashSet::new());
        // a/, a/b/, one.txt, two.txt, top.txt
        assert_eq!(open.len(), 5);
        assert_eq!(open.file_index(0), None);
        assert_eq!(open.file_index(2), Some(0));
        let mut collapsed = HashSet::new();
        collapsed.insert(b"a".to_vec());
        let closed = build(&files, "", true, &collapsed);
        // a/ (collapsed), top.txt
        assert_eq!(closed.len(), 2);
        assert_eq!(closed.file_index(1), Some(2));
    }

    #[test]
    fn list_mode_keeps_the_given_order() {
        let files = vec![
            file("z.txt", ChangeKind::Added),
            file("a.txt", ChangeKind::Added),
        ];
        let rows = build(&files, "", false, &HashSet::new());
        assert_eq!(rows.file_index(0), Some(0));
        assert_eq!(rows.file_index(1), Some(1));
    }

    #[test]
    fn blame_revision_uses_the_parent_for_deleted_files() {
        let selected =
            ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("hex");
        let parent = ObjectId::from_hex(b"2222222222222222222222222222222222222222").expect("hex");
        let mut deleted = file("gone.txt", ChangeKind::Deleted);
        deleted.target = DiffTarget::Commit {
            commit: selected,
            against: Against::FirstParent,
            path: RelaPath::from("gone.txt"),
            previous_path: None,
        };
        assert_eq!(blame_revision(&deleted, selected, Some(parent)), parent);
        assert_eq!(blame_revision(&deleted, selected, None), selected);
        let mut modified = file("kept.txt", ChangeKind::Modified);
        modified.target = DiffTarget::Commit {
            commit: selected,
            against: Against::FirstParent,
            path: RelaPath::from("kept.txt"),
            previous_path: None,
        };
        assert_eq!(blame_revision(&modified, selected, Some(parent)), selected);
    }
}
