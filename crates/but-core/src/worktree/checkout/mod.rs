/// Options for use in [super::safe_checkout_from_head()].
#[derive(Default, Debug, Clone)]
pub struct Options {
    /// If `true`, do not change `HEAD` to the new commit.
    ///
    /// This is typically to be avoided, but may be used if you want to change the HEAD location yourself.
    pub skip_head_update: bool,
    /// If set, use this tree instead of `HEAD^{tree}` as the merge base when
    /// resolving the worktree snapshot against the new HEAD.
    ///
    /// Set this to `HEAD^{tree}` + consumed changes (additive-only) after a
    /// commit/amend so the consumed hunks cancel in the 3-way merge and don't
    /// reappear as uncommitted changes.
    pub merge_base_override: Option<gix::ObjectId>,
    /// Allow checking out GitButler-managed conflicted commits.
    ///
    /// Most callers should keep the default refusal and surface a higher-level
    /// conflict workflow instead. Rebase materialization may opt in when it
    /// intentionally created the conflicted commit it is about to materialize.
    pub allow_conflicted_commit_checkout: bool,
    /// Normally, the 3-way merge between the merge base override, the working
    /// directory, and the new HEAD must not result in a conflict. If this field
    /// is true, such a conflict is allowed; all stages in all conflicts will be
    /// written to the index.
    pub allow_uncommitted_changes_to_conflict_with_new_head: bool,
    /// If `Some(n)` with `n > 0`, untracked files larger than `n` bytes are not hashed when the worktree is
    /// snapshotted for the 3-way merge. They are then unknown to the checkout, which never removes them and refuses
    /// to write a file over one of them ([`UncommittedChangesInTheWay`]). `None` or `Some(0)`: hash all (current behaviour).
    pub untracked_size_limit: Option<u64>,
}

/// The successful outcome of [super::safe_checkout_from_head()] operation.
#[derive(Clone)]
pub struct Outcome {
    /// If `new_head_id` was a commit, these are the ref-edits returned after performing the transaction.
    pub head_update: Option<Vec<gix::refs::transaction::RefEdit>>,
    /// True if a conflict occurred during checkout. This is always false if
    /// [Options::allow_uncommitted_changes_to_conflict_with_new_head] is false.
    pub conflict_occurred: bool,
}

/// Why [super::safe_checkout_from_head()] refused: uncommitted changes would be overwritten.
///
/// It sits below the [`but_error::Context`] with the message, so callers can tell this refusal
/// from other preconditions by downcasting to it.
#[derive(Debug, Clone)]
pub struct UncommittedChangesInTheWay {
    /// The paths that would be overwritten, sorted, deduplicated and not quoted.
    pub paths: Vec<bstr::BString>,
}

impl std::fmt::Display for UncommittedChangesInTheWay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("uncommitted changes are in the way")
    }
}

impl UncommittedChangesInTheWay {
    /// The refusal to overwrite the uncommitted changes at `paths`, which are sorted and deduplicated here.
    pub(in crate::worktree::checkout) fn refusal(mut paths: Vec<bstr::BString>) -> anyhow::Error {
        paths.sort();
        paths.dedup();
        let quoted: Vec<String> = paths.iter().map(|p| format!("{p:?}")).collect();
        anyhow::anyhow!(UncommittedChangesInTheWay { paths }).context(
            but_error::Context::new(format!(
                "Uncommitted files would be overwritten by checkout: {}",
                quoted.join(", ")
            ))
            .with_code(but_error::Code::PreconditionFailed),
        )
    }
}

pub(crate) mod function;
mod utils;
mod write;
