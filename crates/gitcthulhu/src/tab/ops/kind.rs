//! `OpKind`: every tab operation with its processing title (07 §12.6), success toast (07 §12.5) and error title
//! (09 §3.17); new operations use the spec 21 §2.11 texts.

/// One variant per row of the operation catalogue (spec 21 §2.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    StageFile,
    StageFiles,
    UnstageFile,
    UnstageFiles,
    StageAll,
    UnstageAll,
    StageDirectory,
    UnstageDirectory,
    StageHunk,
    StageLine,
    UnstageHunk,
    UnstageLine,
    DiscardFile,
    DiscardHunk,
    DiscardLine,
    DeleteFile,
    ResolveConflict,
    Commit,
    Amend,
    ContinueOperation,
    ResetRepositoryState,
    SkipRebase,
    CheckoutBranch,
    CheckoutRemoteBranch,
    CheckoutCommit,
    CheckoutTag,
    CreateBranch,
    RenameBranch,
    DeleteBranch,
    ChangeUpstream,
    CreateTag,
    DeleteTag,
    Stash,
    ApplyStash,
    PopStash,
    DropStash,
    AddRemote,
    UpdateRemote,
    DeleteRemote,
    AddSubmodule,
    InitSubmodule,
    UpdateSubmodule,
    SyncSubmodule,
    DeinitSubmodule,
    DeleteSubmodule,
    Merge,
    Rebase,
    RebaseInteractive,
    CherryPickCommit,
    RevertCommit,
    ResetToCommit,
    Fetch,
    Pull,
    Push,
    DeleteRemoteBranch,
    PushTag,
    DeleteRemoteTag,
    SaveAuthor,
    WriteCommitGraph,
    AddWorktree,
    RemoveWorktree,
    PruneWorktrees,
    LockWorktree,
    UnlockWorktree,
    Undo,
    Redo,
    RestoreTo,
    ClearUndoHistory,
    RecoverUndo,
    ApplyUndoSettings,
}

impl OpKind {
    /// Every kind (tests).
    pub const ALL: [OpKind; 70] = {
        use OpKind::*;
        [
            StageFile,
            StageFiles,
            UnstageFile,
            UnstageFiles,
            StageAll,
            UnstageAll,
            StageDirectory,
            UnstageDirectory,
            StageHunk,
            StageLine,
            UnstageHunk,
            UnstageLine,
            DiscardFile,
            DiscardHunk,
            DiscardLine,
            DeleteFile,
            ResolveConflict,
            Commit,
            Amend,
            ContinueOperation,
            ResetRepositoryState,
            SkipRebase,
            CheckoutBranch,
            CheckoutRemoteBranch,
            CheckoutCommit,
            CheckoutTag,
            CreateBranch,
            RenameBranch,
            DeleteBranch,
            ChangeUpstream,
            CreateTag,
            DeleteTag,
            Stash,
            ApplyStash,
            PopStash,
            DropStash,
            AddRemote,
            UpdateRemote,
            DeleteRemote,
            AddSubmodule,
            InitSubmodule,
            UpdateSubmodule,
            SyncSubmodule,
            DeinitSubmodule,
            DeleteSubmodule,
            Merge,
            Rebase,
            RebaseInteractive,
            CherryPickCommit,
            RevertCommit,
            ResetToCommit,
            Fetch,
            Pull,
            Push,
            DeleteRemoteBranch,
            PushTag,
            DeleteRemoteTag,
            SaveAuthor,
            WriteCommitGraph,
            AddWorktree,
            RemoveWorktree,
            PruneWorktrees,
            LockWorktree,
            UnlockWorktree,
            Undo,
            Redo,
            RestoreTo,
            ClearUndoHistory,
            RecoverUndo,
            ApplyUndoSettings,
        ]
    };

    /// (processing title, success toast, error title).
    fn texts(self) -> (&'static str, Option<&'static str>, &'static str) {
        use OpKind::*;
        match self {
            StageFile => ("Staging file", None, "File stage failed"),
            StageFiles => ("Staging files", None, "File stage failed"),
            UnstageFile => ("Unstaging file", None, "File unstage failed"),
            UnstageFiles => ("Unstaging files", None, "File unstage failed"),
            StageAll => ("Staging all files", None, "Staging all the files failed"),
            UnstageAll => (
                "Unstaging all files",
                None,
                "Unstaging all the files failed",
            ),
            StageDirectory => ("Staging directory", None, "File stage failed"),
            UnstageDirectory => ("Unstaging directory", None, "File unstage failed"),
            StageHunk => ("Staging hunk", None, "File stage failed"),
            StageLine => (
                "Staging line",
                Some("File line staged"),
                "File line stage failed",
            ),
            UnstageHunk => ("Unstaging hunk", None, "Hunk unstage failed"),
            UnstageLine => (
                "Unstaging line",
                Some("File line unstaged"),
                "File line unstage failed",
            ),
            DiscardFile => (
                "Discarding file",
                Some("File discarded"),
                "Discard file failed",
            ),
            DiscardHunk => ("Discarding hunk", None, "Discard hunk failed"),
            DiscardLine => ("Discarding line", None, "Discard line failed"),
            DeleteFile => ("Deleting file", Some("File deleted"), "Delete file failed"),
            ResolveConflict => (
                "Resolving conflict",
                Some("Conflict resolved"),
                "Resolving conflict failed",
            ),
            Commit => ("Committing", None, "Commit failed"),
            Amend => ("Committing", None, "Commit failed"),
            ContinueOperation => (
                "Continuing rebase",
                Some("Rebase continued"),
                "Could not continue rebase",
            ),
            ResetRepositoryState => (
                "Resetting repository state",
                Some("Repository state reset"),
                "Error",
            ),
            SkipRebase => (
                "Skipping rebase",
                Some("Rebase step skipped"),
                "Could not skip rebase step",
            ),
            CheckoutBranch => (
                "Checking out branch",
                Some("Branch checked out"),
                "Branch checkout failed",
            ),
            CheckoutRemoteBranch => (
                "Checking out remote branch",
                Some("Remote branch checked out"),
                "Remote branch checkout failed",
            ),
            CheckoutCommit => (
                "Checking out commit",
                Some("Commit checked out"),
                "Checkout commit failed",
            ),
            CheckoutTag => (
                "Checking out tag",
                Some("Tag checked out"),
                "Could not checkout tag's commit",
            ),
            CreateBranch => (
                "Creating branch",
                Some("Branch created"),
                "Could not create the new branch",
            ),
            RenameBranch => (
                "Renaming branch",
                Some("Branch renamed"),
                "Could not rename the branch",
            ),
            DeleteBranch => (
                "Deleting branch",
                Some("Branch deleted"),
                "Could not delete the branch",
            ),
            ChangeUpstream => (
                "Changing branch upstream",
                Some("Upstream branch changed"),
                "Upstream branch change failed",
            ),
            CreateTag => ("Creating tag", Some("Tag created"), "Create tag failed"),
            DeleteTag => ("Deleting tag", Some("Tag deleted"), "Could not delete tag"),
            Stash => ("Stashing", Some("Changes stashed"), "Stash failed"),
            ApplyStash => (
                "Applying stash",
                Some("Stash applied"),
                "Apply stash failed",
            ),
            PopStash => ("Popping stash", Some("Stash popped"), "Pop stash failed"),
            DropStash => (
                "Deleting stash",
                Some("Stash deleted"),
                "Delete stash failed",
            ),
            AddRemote => (
                "Adding remote",
                Some("Remote added"),
                "Adding remote failed",
            ),
            UpdateRemote => (
                "Updating remote",
                Some("Remote updated"),
                "Updating remote failed",
            ),
            DeleteRemote => (
                "Deleting remote",
                Some("Remote deleted"),
                "Deleting remote failed",
            ),
            AddSubmodule => (
                "Adding submodule",
                Some("Submodule added"),
                "Add submodule failed",
            ),
            InitSubmodule => (
                "Initializing submodule",
                Some("Submodule initialized"),
                "Init submodule failed",
            ),
            UpdateSubmodule => (
                "Updating submodule",
                Some("Submodule updated"),
                "Update submodule failed",
            ),
            SyncSubmodule => (
                "Synchronizing submodule",
                Some("Submodule synchronized"),
                "Sync submodule failed",
            ),
            DeinitSubmodule => (
                "Deinitializing submodule",
                Some("Submodule deinitialized"),
                "Deinit submodule failed",
            ),
            DeleteSubmodule => (
                "Deleting submodule",
                Some("Submodule deleted"),
                "Delete submodule failed",
            ),
            Merge => ("Merging branch", Some("Merge completed"), "Merge failed"),
            Rebase => ("Rebasing branch", Some("Rebase completed"), "Rebase failed"),
            RebaseInteractive => (
                "Rebasing interactively",
                Some("Interactive rebase completed"),
                "Rebase interactive failed",
            ),
            CherryPickCommit => (
                "Cherry picking commit",
                Some("Commit cherry-picked"),
                "Commit cherry-pick failed",
            ),
            RevertCommit => (
                "Reverting commit",
                Some("Commit reverted"),
                "Commit revert failed",
            ),
            ResetToCommit => (
                "Resetting to commit",
                Some("Reset to commit completed"),
                "Reset to commit failed",
            ),
            Fetch => ("Fetching", Some("Fetch completed"), "Fetch failed"),
            Pull => ("Pulling", Some("Pull completed"), "Pull failed"),
            Push => ("Pushing", Some("Push completed"), "Push failed"),
            DeleteRemoteBranch => (
                "Deleting remote branch",
                Some("Remote branch deleted"),
                "Deleting remote branch failed",
            ),
            PushTag => ("Pushing tag", Some("Tag pushed"), "Push tag failed"),
            DeleteRemoteTag => (
                "Deleting remote tag",
                Some("Remote tag deleted"),
                "Deleting remote tag failed",
            ),
            SaveAuthor => (
                "Saving author",
                Some("Author updated"),
                "Saving author failed",
            ),
            WriteCommitGraph => (
                "Writing commit-graph",
                Some("Commit-graph written"),
                "Writing commit-graph failed",
            ),
            AddWorktree => (
                "Adding worktree",
                Some("Worktree added"),
                "Adding worktree failed",
            ),
            RemoveWorktree => (
                "Removing worktree",
                Some("Worktree removed"),
                "Removing worktree failed",
            ),
            PruneWorktrees => (
                "Pruning worktrees",
                Some("Worktrees pruned"),
                "Pruning worktrees failed",
            ),
            LockWorktree => (
                "Locking worktree",
                Some("Worktree locked"),
                "Locking worktree failed",
            ),
            UnlockWorktree => (
                "Unlocking worktree",
                Some("Worktree unlocked"),
                "Unlocking worktree failed",
            ),
            // The success toasts of restores are dynamic ("Undone: {title}").
            Undo => ("Undoing", None, "Undo failed"),
            Redo => ("Redoing", None, "Redo failed"),
            RestoreTo => ("Restoring", Some("Repository restored"), "Restore failed"),
            ClearUndoHistory => (
                "Clearing undo history",
                Some("Undo history cleared"),
                "Clearing undo history failed",
            ),
            RecoverUndo => (
                "Finishing the interrupted undo",
                Some("The interrupted undo was resolved"),
                "Recovering the interrupted undo failed",
            ),
            ApplyUndoSettings => (
                "Applying undo settings",
                None,
                "Applying undo settings failed",
            ),
        }
    }

    /// Title of the processing overlay (07 §12.6).
    pub fn processing_title(self) -> &'static str {
        self.texts().0
    }

    /// Positive toast after success (07 §12.5); `None` = no toast.
    pub fn success_toast(self) -> Option<&'static str> {
        self.texts().1
    }

    /// Title of the error dialog (09 §3.17).
    pub fn error_title(self) -> &'static str {
        self.texts().2
    }

    /// What a successful run is called in texts that need one even without a success toast (post-hook warnings).
    pub fn completed_text(self) -> &'static str {
        match self {
            OpKind::Commit => "Commit created",
            OpKind::Amend => "Commit amended",
            other => other.success_toast().unwrap_or("Operation completed"),
        }
    }

    /// The operation's name at the start of a sentence ("Merge stopped with conflicts").
    pub fn operation_name(self) -> &'static str {
        use OpKind::*;
        match self {
            Merge => "Merge",
            Rebase => "Rebase",
            RebaseInteractive => "Interactive rebase",
            CherryPickCommit => "Cherry-pick",
            RevertCommit => "Revert",
            Pull => "Pull",
            ApplyStash | PopStash => "Applying the stash",
            CheckoutBranch | CheckoutRemoteBranch | CheckoutCommit | CheckoutTag => "Checkout",
            ContinueOperation => "Continuing",
            SkipRebase => "Skipping",
            _ => "The operation",
        }
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn every_kind_has_titles() {
        for kind in OpKind::ALL {
            assert!(
                !kind.processing_title().is_empty() && !kind.error_title().is_empty(),
                "{kind:?} needs a processing and an error title"
            );
        }
        assert_eq!(OpKind::Fetch.success_toast(), Some("Fetch completed"));
        assert_eq!(OpKind::Commit.error_title(), "Commit failed");
        assert_eq!(
            OpKind::ApplyUndoSettings.success_toast(),
            None,
            "applying undo settings shows no toast"
        );
    }
}
