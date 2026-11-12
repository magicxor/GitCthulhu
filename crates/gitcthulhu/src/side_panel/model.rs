//! The flattened row model of the side panel (owned by G3 after G0b; spec 21 §4.3). Rows are rebuilt (pure, UI
//! thread) on refs/submodules/worktrees changes, filter changes and expansion changes; the Worktrees section exists
//! only when `worktrees::section_enabled(cx)` (milestone 1: never).

use std::collections::{HashMap, HashSet};

use crate::backend::{
    ByteSlice as _, FullName, LocalBranch, ObjectId, RefList, RemoteInfo, StashInfo, SubmoduleInfo,
    SubmoduleState, TagInfo, WorktreeInfo,
};
use crate::tab::ui::SideExpansion;

/// A section of the side panel, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    LocalBranches,
    Remotes,
    Tags,
    Stashes,
    Submodules,
    Worktrees,
}

impl Section {
    /// The sections in display order (07 §6.2).
    pub const ALL: [Section; 6] = [
        Section::LocalBranches,
        Section::Remotes,
        Section::Tags,
        Section::Stashes,
        Section::Submodules,
        Section::Worktrees,
    ];

    /// The header title (07 §6.3).
    pub fn title(self) -> &'static str {
        match self {
            Section::LocalBranches => "Local branches",
            Section::Remotes => "Remotes",
            Section::Tags => "Tags",
            Section::Stashes => "Stashes",
            Section::Submodules => "Submodules",
            Section::Worktrees => "Worktrees",
        }
    }

    /// A short id used in element ids and tests.
    pub fn key(self) -> &'static str {
        match self {
            Section::LocalBranches => "local",
            Section::Remotes => "remotes",
            Section::Tags => "tags",
            Section::Stashes => "stashes",
            Section::Submodules => "submodules",
            Section::Worktrees => "worktrees",
        }
    }

    /// Whether the header shows a "+" button instead of the count while hovered (07 §6.3; the Worktrees "+" is
    /// D0.7's).
    pub fn has_add_button(self) -> bool {
        matches!(
            self,
            Section::Remotes | Section::Submodules | Section::Worktrees
        )
    }

    /// Whether the section is expanded in `expanded`.
    pub fn is_expanded(self, expanded: &SideExpansion) -> bool {
        match self {
            Section::LocalBranches => expanded.local,
            Section::Remotes => expanded.remotes,
            Section::Tags => expanded.tags,
            Section::Stashes => expanded.stashes,
            Section::Submodules => expanded.submodules,
            Section::Worktrees => expanded.worktrees,
        }
    }

    /// Flips the section's expansion in `expanded`.
    pub fn toggle(self, expanded: &mut SideExpansion) {
        let flag = match self {
            Section::LocalBranches => &mut expanded.local,
            Section::Remotes => &mut expanded.remotes,
            Section::Tags => &mut expanded.tags,
            Section::Stashes => &mut expanded.stashes,
            Section::Submodules => &mut expanded.submodules,
            Section::Worktrees => &mut expanded.worktrees,
        };
        *flag = !*flag;
    }
}

/// One row of the flattened list; indexes point into the model's ref lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SideRow {
    Header {
        section: Section,
        count: usize,
        expanded: bool,
    },
    LocalBranch {
        ix: usize,
    },
    Remote {
        ix: usize,
        collapsed: bool,
    },
    RemoteBranch {
        remote: usize,
        ix: usize,
    },
    Tag {
        ix: usize,
    },
    Stash {
        ix: usize,
    },
    Submodule {
        ix: usize,
    },
    Worktree {
        ix: usize,
    },
}

/// Everything the row list depends on.
pub struct RowInputs<'a> {
    /// The loaded refs; `None` before the first load.
    pub refs: Option<&'a RefList>,
    /// The loaded submodules.
    pub submodules: &'a [SubmoduleInfo],
    /// The loaded worktrees, `Some` only when the Worktrees section is enabled.
    pub worktrees: Option<&'a [WorktreeInfo]>,
    /// The filter text (any case).
    pub filter: &'a str,
    /// Which sections are expanded.
    pub expanded: &'a SideExpansion,
    /// Names of the remotes whose branch lists are collapsed.
    pub remotes_collapsed: &'a HashSet<String>,
}

/// Case-insensitive substring match; `needle_lower` must already be lower-case (07 §6.2).
pub fn matches_filter(needle_lower: &str, haystack: &str) -> bool {
    needle_lower.is_empty() || haystack.to_lowercase().contains(needle_lower)
}

/// The first line of a stash message (`stash.shortMessage`).
pub fn stash_title(message: &str) -> &str {
    message.lines().next().unwrap_or("")
}

/// Builds the flattened list: five (six with worktrees) headers, each followed by its rows when expanded. Sections
/// are always present, even with zero items; counts are the filtered counts (the Remotes count is the number of
/// remotes, whose headers stay visible whatever the filter).
pub fn build_rows(inputs: &RowInputs<'_>) -> Vec<SideRow> {
    let needle = inputs.filter.to_lowercase();
    let mut rows = Vec::new();
    let (local_all, remotes, tag_all, stash_all): (
        &[LocalBranch],
        &[RemoteInfo],
        &[TagInfo],
        &[StashInfo],
    ) = match inputs.refs {
        Some(refs) => (&refs.local, &refs.remotes, &refs.tags, &refs.stashes),
        None => (&[], &[], &[], &[]),
    };

    // Local branches: the current one first, then by name, case-insensitive; filtered on the full ref name.
    let mut local: Vec<usize> = (0..local_all.len())
        .filter(|&ix| {
            needle.is_empty() || matches_filter(&needle, &local_all[ix].full.as_bstr().to_string())
        })
        .collect();
    local.sort_by_cached_key(|&ix| {
        let b = &local_all[ix];
        (!b.is_head, b.name.to_lowercase())
    });
    header(
        &mut rows,
        Section::LocalBranches,
        local.len(),
        inputs.expanded,
    );
    if inputs.expanded.local {
        rows.extend(local.into_iter().map(|ix| SideRow::LocalBranch { ix }));
    }

    // Remotes: the headers stay visible, their branches are filtered on the simple name.
    header(&mut rows, Section::Remotes, remotes.len(), inputs.expanded);
    if inputs.expanded.remotes {
        for (remote_ix, remote) in remotes.iter().enumerate() {
            let collapsed = inputs.remotes_collapsed.contains(&remote.name);
            rows.push(SideRow::Remote {
                ix: remote_ix,
                collapsed,
            });
            if !collapsed {
                rows.extend(
                    remote
                        .branches
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| matches_filter(&needle, &b.name))
                        .map(|(ix, _)| SideRow::RemoteBranch {
                            remote: remote_ix,
                            ix,
                        }),
                );
            }
        }
    }

    let tags: Vec<usize> = (0..tag_all.len())
        .filter(|&ix| matches_filter(&needle, &tag_all[ix].name))
        .collect();
    header(&mut rows, Section::Tags, tags.len(), inputs.expanded);
    if inputs.expanded.tags {
        rows.extend(tags.into_iter().map(|ix| SideRow::Tag { ix }));
    }

    let stashes: Vec<usize> = (0..stash_all.len())
        .filter(|&ix| matches_filter(&needle, &stash_all[ix].message))
        .collect();
    header(&mut rows, Section::Stashes, stashes.len(), inputs.expanded);
    if inputs.expanded.stashes {
        rows.extend(stashes.into_iter().map(|ix| SideRow::Stash { ix }));
    }

    let submodules: Vec<usize> = (0..inputs.submodules.len())
        .filter(|&ix| {
            needle.is_empty() || matches_filter(&needle, &inputs.submodules[ix].path.to_str_lossy())
        })
        .collect();
    header(
        &mut rows,
        Section::Submodules,
        submodules.len(),
        inputs.expanded,
    );
    if inputs.expanded.submodules {
        rows.extend(submodules.into_iter().map(|ix| SideRow::Submodule { ix }));
    }

    if let Some(worktrees) = inputs.worktrees {
        header(
            &mut rows,
            Section::Worktrees,
            worktrees.len(),
            inputs.expanded,
        );
        if inputs.expanded.worktrees {
            rows.extend((0..worktrees.len()).map(|ix| SideRow::Worktree { ix }));
        }
    }
    rows
}

fn header(rows: &mut Vec<SideRow>, section: Section, count: usize, expanded: &SideExpansion) {
    rows.push(SideRow::Header {
        section,
        count,
        expanded: section.is_expanded(expanded),
    });
}

/// For each local branch (index into `RefList::local`) whose upstream has a tracking ref: the (local target,
/// tracking target) pair `RepoTab::request_ahead_behind` and `RepoModel::ahead_behind` use.
pub fn tracking_pairs_by_branch(refs: &RefList) -> HashMap<usize, (ObjectId, ObjectId)> {
    let wanted: HashMap<&FullName, Vec<usize>> = {
        let mut map: HashMap<&FullName, Vec<usize>> = HashMap::new();
        for (ix, branch) in refs.local.iter().enumerate() {
            if let Some(tracking) = branch.upstream.as_ref().and_then(|u| u.tracking.as_ref()) {
                map.entry(tracking).or_default().push(ix);
            }
        }
        map
    };
    let mut pairs = HashMap::new();
    if wanted.is_empty() {
        return pairs;
    }
    for branch in refs.remotes.iter().flat_map(|r| &r.branches) {
        if let Some(locals) = wanted.get(&branch.full) {
            for &ix in locals {
                pairs.insert(ix, (refs.local[ix].target, branch.target));
            }
        }
    }
    pairs
}

/// The letter and full name of a submodule state (07 §6.5: M/U/I/R with a tooltip of the state's name).
pub fn submodule_state(state: SubmoduleState) -> (&'static str, &'static str) {
    match state {
        SubmoduleState::Missing => ("M", "MISSING"),
        SubmoduleState::Uninitialized => ("U", "UNINITIALIZED"),
        SubmoduleState::Initialized => ("I", "INITIALIZED"),
        SubmoduleState::CheckedOut => ("R", "REV_CHECKED_OUT"),
    }
}

/// Whether a submodule can be opened in a tab (INITIALIZED or REV_CHECKED_OUT).
pub fn submodule_is_valid(state: SubmoduleState) -> bool {
    matches!(
        state,
        SubmoduleState::Initialized | SubmoduleState::CheckedOut
    )
}
