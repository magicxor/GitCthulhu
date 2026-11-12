use std::{fmt::Display, path::PathBuf};

use gix::bstr::BString;

/// The checkout of a repository in which files changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    /// The opened checkout (the main worktree, or the linked worktree that was opened).
    Main,
    /// The linked worktree of this name, i.e. its directory name under `$GIT_COMMON_DIR/worktrees/`.
    Linked(BString),
}

/// A linked worktree to watch alongside the main worktree.
#[derive(Debug, Clone)]
pub struct LinkedWorktree {
    /// The directory name under `$GIT_COMMON_DIR/worktrees/`.
    pub name: BString,
    /// The checkout directory.
    pub workdir: PathBuf,
}

/// An event for internal use, representing file system changes.
#[derive(Debug)]
pub enum InternalEvent {
    // From file monitor
    /// Paths relative to the git dir of the checkout.
    GitFilesChange(Checkout, Vec<PathBuf>),
    /// Paths relative to the common git dir, reported only if the watched checkout is a linked worktree
    /// (whose refs, `packed-refs` and `config` live in the common dir).
    CommonGitFilesChange(Vec<PathBuf>),
    /// Paths relative to the working directory of the checkout.
    ProjectFilesChange(Checkout, Vec<PathBuf>),
    /// Part of the watching does not work: the operating system's watch limit was reached, a directory could not
    /// be watched, or the watcher reported an error (for example lost events). Changes may be missed; whoever
    /// consumes the events should refresh by other means.
    WatchError(String),
}

impl Display for InternalEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InternalEvent::GitFilesChange(checkout, paths) => {
                write!(
                    f,
                    "GitFileChange({:?}, {})",
                    checkout,
                    comma_separated_paths(paths)
                )
            }
            InternalEvent::CommonGitFilesChange(paths) => {
                write!(f, "CommonGitFileChange({})", comma_separated_paths(paths))
            }
            InternalEvent::ProjectFilesChange(checkout, paths) => {
                write!(
                    f,
                    "ProjectFileChange({:?}, {})",
                    checkout,
                    comma_separated_paths(paths)
                )
            }
            InternalEvent::WatchError(message) => write!(f, "WatchError({message})"),
        }
    }
}

fn comma_separated_paths(paths: &[PathBuf]) -> String {
    const MAX_LISTING: usize = 5;
    let listing = paths
        .iter()
        .take(MAX_LISTING)
        .filter_map(|path| path.to_str())
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = paths.len().saturating_sub(MAX_LISTING);
    if remaining > 0 {
        format!("{listing} […{remaining} more]")
    } else {
        listing
    }
}
