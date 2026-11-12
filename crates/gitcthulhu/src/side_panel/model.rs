//! The flattened row model of the side panel (owned by G3 after G0b; spec 21 §4.3). Rows are rebuilt (pure, UI
//! thread) on refs/submodules/worktrees changes, filter changes and expansion changes; the Worktrees section exists
//! only when `worktrees::section_enabled(cx)` (milestone 1: never).

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
