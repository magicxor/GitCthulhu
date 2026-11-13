//! The rows of one changes list: conflict prioritisation, the file filter and the list or tree flattening
//! (08 §4.3, §4.4). Pure data, rebuilt only when the status, the filter, the view mode or the collapsed set
//! change, never per frame.

use std::collections::{HashMap, HashSet};

use crate::backend::{ByteSlice as _, ChangeKind, RelaPath, StatusEntry};
use crate::widgets::file_tree::{TreeRow, flatten_list, flatten_tree, visible_file_order};

/// Collapses duplicate paths of one list: the CONFLICTING entry wins (08 §4.4, `prioritizeConflicts`); otherwise
/// the first entry stays. The order of first occurrence is kept.
pub fn prioritize_conflicts(entries: &[StatusEntry]) -> Vec<StatusEntry> {
    let mut slot_of: HashMap<&[u8], usize> = HashMap::with_capacity(entries.len());
    let mut out: Vec<StatusEntry> = Vec::with_capacity(entries.len());
    for entry in entries {
        match slot_of.get(entry.path.as_slice()) {
            Some(&slot) => {
                if entry.kind == ChangeKind::Conflicted && out[slot].kind != ChangeKind::Conflicted
                {
                    out[slot] = entry.clone();
                }
            }
            None => {
                slot_of.insert(entry.path.as_slice(), out.len());
                out.push(entry.clone());
            }
        }
    }
    out
}

/// Whether `path` matches the filter text: case-insensitive substring of the full relative path. A blank filter
/// matches everything.
pub fn matches_filter(path: &RelaPath, filter_lower: &str) -> bool {
    filter_lower.is_empty() || path.to_str_lossy().to_lowercase().contains(filter_lower)
}

/// The displayed rows of a list.
#[derive(Debug)]
pub struct ListData {
    /// The entries behind the rows: conflict-prioritised and filtered, in the order the backend sorted them.
    pub entries: Vec<StatusEntry>,
    /// List rows, or tree rows with directories (`TreeRow::File::index` points into `entries`).
    pub rows: Vec<TreeRow>,
    /// Tree mode.
    pub tree: bool,
}

impl ListData {
    /// Builds the rows of `entries` for `filter` (`None`: the search field is hidden, nothing is filtered), the
    /// view mode and the collapsed directories.
    pub fn build(
        entries: &[StatusEntry],
        filter: Option<&str>,
        tree: bool,
        collapsed: &HashSet<Vec<u8>>,
    ) -> Self {
        let mut entries = prioritize_conflicts(entries);
        if let Some(filter) = filter {
            let lower = filter.trim().to_lowercase();
            if !lower.is_empty() {
                entries.retain(|e| matches_filter(&e.path, &lower));
            }
        }
        let paths: Vec<&[u8]> = entries.iter().map(|e| e.path.as_slice()).collect();
        let rows = if tree {
            flatten_tree(&paths, collapsed)
        } else {
            flatten_list(&paths)
        };
        Self {
            entries,
            rows,
            tree,
        }
    }

    /// The entry of a file row.
    pub fn entry_of_row(&self, row: usize) -> Option<&StatusEntry> {
        match self.rows.get(row)? {
            TreeRow::File { index, .. } => self.entries.get(*index),
            TreeRow::Dir { .. } => None,
        }
    }

    /// The paths of the visible file rows in display order: the order Shift-click ranges use (08 §4.5).
    pub fn visible_paths(&self) -> Vec<RelaPath> {
        visible_file_order(&self.rows)
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .map(|e| e.path.clone())
            .collect()
    }

    /// The entries at or below the directory `dir`, in display order.
    pub fn entries_under(&self, dir: &[u8]) -> Vec<&StatusEntry> {
        self.entries
            .iter()
            .filter(|e| {
                let p = e.path.as_slice();
                p.len() > dir.len() && p.starts_with(dir) && p[dir.len()] == b'/'
            })
            .collect()
    }

    /// The entries whose paths are in `selected`, in display order.
    pub fn entries_of<'a>(
        &'a self,
        selected: &'a indexmap::IndexSet<RelaPath>,
    ) -> Vec<&'a StatusEntry> {
        self.entries
            .iter()
            .filter(|e| selected.contains(&e.path))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;
    use crate::test_support::seed::entry;

    fn paths(data: &ListData) -> Vec<String> {
        data.entries.iter().map(|e| e.path.to_string()).collect()
    }

    #[test]
    fn conflicting_entry_wins_a_duplicate_path() {
        let entries = [
            entry("a.txt", ChangeKind::Modified),
            entry("a.txt", ChangeKind::Conflicted),
            entry("b.txt", ChangeKind::Modified),
        ];
        let out = prioritize_conflicts(&entries);
        assert_eq!(out.len(), 2, "one row per path");
        assert_eq!(out[0].kind, ChangeKind::Conflicted, "the conflict wins");
        assert_eq!(out[1].path.to_string(), "b.txt");
    }

    #[test]
    fn filter_is_a_case_insensitive_substring_of_the_path() {
        let entries = [
            entry("src/Main.rs", ChangeKind::Modified),
            entry("docs/readme.md", ChangeKind::Added),
        ];
        let data = ListData::build(&entries, Some("MAIN"), false, &HashSet::new());
        assert_eq!(paths(&data), ["src/Main.rs"]);
        let data = ListData::build(&entries, Some("docs/"), false, &HashSet::new());
        assert_eq!(paths(&data), ["docs/readme.md"], "the full path is matched");
        let data = ListData::build(&entries, None, false, &HashSet::new());
        assert_eq!(data.entries.len(), 2, "a hidden filter does not filter");
        let data = ListData::build(&entries, Some("  "), false, &HashSet::new());
        assert_eq!(data.entries.len(), 2, "a blank filter matches everything");
    }

    #[test]
    fn tree_rows_and_directory_entries() {
        let entries = [
            entry("src/a/x.rs", ChangeKind::Modified),
            entry("src/b.rs", ChangeKind::Modified),
            entry("src2/c.rs", ChangeKind::Modified),
        ];
        let data = ListData::build(&entries, None, true, &HashSet::new());
        let under: Vec<String> = data
            .entries_under(b"src")
            .iter()
            .map(|e| e.path.to_string())
            .collect();
        assert_eq!(under, ["src/a/x.rs", "src/b.rs"], "src2 is not under src");
        assert_eq!(
            data.visible_paths()
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>(),
            ["src/a/x.rs", "src/b.rs", "src2/c.rs"],
            "directories are not in the range order"
        );
    }
}
