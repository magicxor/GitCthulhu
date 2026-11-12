//! Commit operations of the catalogue (spec 21 §2.11; owned by G5): `git commit` (also amend and the concluding
//! commit of a merge, cherry-pick or revert), and continue / abort / skip of the operation in progress.

use std::rc::Rc;

use gpui_kit::{Context, SharedString, Window};

use super::{Cancellable, Handled, OpKind, OpSpec};
use crate::app_view::show_toast;
use crate::backend::UntrackedCapture;
use crate::backend::{
    CommitOptions, ContinueOutcome, GitErrorKind, InProgress, OperationDescriptor, OperationKind,
    RebaseOutcome, RebaseStopReason, RefreshSet, Scope, git,
};
use crate::dialogs::author::IdentityChoice;
use crate::notifications::ToastKind;
use crate::tab::RepoTab;

/// The summary line of a commit message (oplog titles).
fn summary(message: &str) -> &str {
    message.lines().next().unwrap_or_default().trim()
}

/// What the operation in progress is called in titles and toasts.
fn operation_name(state: Option<InProgress>) -> &'static str {
    match state {
        Some(InProgress::Merge) => "merge",
        Some(InProgress::CherryPick | InProgress::CherryPickSequence) => "cherry-pick",
        Some(InProgress::Revert | InProgress::RevertSequence) => "revert",
        Some(
            InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase,
        ) => "rebase",
        Some(InProgress::ApplyMailbox) => "patch series",
        Some(InProgress::Bisect) | None => "operation",
    }
}

/// `Rebase stopped with conflicts` and the like, for a step of a rebase that stopped again.
fn stop_text(reason: RebaseStopReason) -> (ToastKind, &'static str) {
    match reason {
        RebaseStopReason::Conflict => (ToastKind::Warning, "Rebase stopped with conflicts"),
        RebaseStopReason::Edit => (ToastKind::Positive, "Rebase stopped to edit a commit"),
        RebaseStopReason::Break => (ToastKind::Positive, "Rebase paused at a break"),
        RebaseStopReason::ExecFailed => (ToastKind::Warning, "A rebase command failed"),
        RebaseStopReason::ResolvedNeedsContinue | RebaseStopReason::Unknown => {
            (ToastKind::Positive, "Rebase stopped")
        }
    }
}

impl RepoTab {
    fn in_progress(&self, cx: &Context<Self>) -> Option<InProgress> {
        self.ctx
            .model
            .read(cx)
            .state
            .value
            .as_ref()
            .and_then(|s| s.in_progress)
    }

    /// ★ `git commit` (hooks included): `Commit "{summary}"` / `Amend "{summary}"` snapshot, cancellable with a
    /// warning, "Retry without hooks" after a failing pre hook. When Git reports an unknown identity the
    /// Commit-author dialog opens and the commit is retried with the chosen identity. Refresh: STATUS | LOG |
    /// BRANCHES | REPO_STATE.
    pub fn commit(
        &mut self,
        opts: CommitOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.commit_inner(opts, false, window, cx)
    }

    /// Amends HEAD at a rebase `edit` stop and then continues the rebase (05 §3.3: the GUI commits first, then
    /// calls `--continue`). Same operation as [`Self::commit`]; the rebase continues after success.
    pub(crate) fn amend_then_continue(
        &mut self,
        opts: CommitOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.commit_inner(opts, true, window, cx)
    }

    fn commit_inner(
        &mut self,
        opts: CommitOptions,
        continue_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (kind, oplog_kind, verb) = if opts.amend {
            (OpKind::Amend, OperationKind::Amend, "Amend")
        } else {
            (OpKind::Commit, OperationKind::Commit, "Commit")
        };
        let descriptor = OperationDescriptor {
            kind: oplog_kind,
            title: format!("{verb} \"{}\"", summary(&opts.message)),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let retry_opts = CommitOptions {
            skip_hooks: true,
            ..opts.clone()
        };
        let identity_opts = opts.clone();
        let mut spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::commit::commit(repo, env.perm(), ctx, &opts)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(
            RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE,
        )
        .on_error(move |tab, failure, window, cx| {
            if failure.git_kind() != Some(GitErrorKind::IdentityUnknown) {
                return Handled::No;
            }
            ask_identity_and_commit(tab, identity_opts, continue_after, window, cx);
            Handled::Yes
        })
        .retry_without_hooks(move |tab, window, cx| {
            tab.commit_inner(retry_opts, continue_after, window, cx);
        });
        if continue_after {
            spec = spec.then(|tab, _, window, cx| {
                tab.continue_operation(None, window, cx);
            });
        }
        self.run_op(spec, window, cx)
    }

    /// `commit` with explicit options (empty-pick "Commit empty", identity overrides); same backend call.
    pub fn commit_with(
        &mut self,
        opts: CommitOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.commit(opts, window, cx)
    }

    /// `--continue` of the operation in progress (`merge::continue_operation`); `message` is written to the
    /// operation's message file first (merge, cherry-pick, revert) or used for a stopped rebase step. Snapshot
    /// `Continue {operation}`, cancellable with a warning. A rebase that stops again is reported in a toast.
    /// Refresh: OPERATION_STATE.
    pub fn continue_operation(
        &mut self,
        message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = operation_name(self.in_progress(cx));
        let descriptor = OperationDescriptor {
            kind: OperationKind::ContinueOperation,
            title: format!("Continue {name}"),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::ContinueOperation, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::merge::continue_operation(repo, env.perm(), ctx, message.as_deref())
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .toast("")
        .then(move |tab, outcome, _window, cx| tab.toast_step_outcome(name, outcome, cx));
        self.run_op(spec, window, cx)
    }

    /// `--abort` of the operation in progress (`merge::abort_operation`): Git restores the state from before the
    /// operation. Snapshot `Abort {operation}`, cancellable with a warning. Refresh: OPERATION_STATE.
    pub fn abort_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = operation_name(self.in_progress(cx));
        let descriptor = OperationDescriptor {
            kind: OperationKind::AbortOperation,
            title: format!("Abort {name}"),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::ResetRepositoryState, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::merge::abort_operation(repo, env.perm(), ctx)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE);
        self.run_op(spec, window, cx)
    }

    /// `--skip` of the operation in progress (`merge::skip_operation`): drops the current step. Snapshot `Skip
    /// commit`, cancellable with a warning. Refresh: OPERATION_STATE.
    pub fn skip_operation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = operation_name(self.in_progress(cx));
        let descriptor = OperationDescriptor {
            kind: OperationKind::SkipOperation,
            title: "Skip commit".to_owned(),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::SkipRebase, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::merge::skip_operation(repo, env.perm(), ctx)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .toast("")
        .then(move |tab, outcome, _window, cx| tab.toast_step_outcome(name, outcome, cx));
        self.run_op(spec, window, cx)
    }

    /// The toast after a continue or skip: completion, or why a rebase stopped again.
    fn toast_step_outcome(
        &mut self,
        name: &'static str,
        outcome: &ContinueOutcome,
        cx: &mut Context<Self>,
    ) {
        let (kind, text): (ToastKind, SharedString) = match outcome {
            ContinueOutcome::Rebase(RebaseOutcome::Stopped { reason, .. }) => {
                let (kind, text) = stop_text(*reason);
                (kind, text.into())
            }
            ContinueOutcome::Rebase(RebaseOutcome::Completed { .. }) => {
                (ToastKind::Positive, "Rebase completed".into())
            }
            ContinueOutcome::Completed => (ToastKind::Positive, format!("{name} completed").into()),
        };
        let text = capitalize(&text);
        show_toast(cx, Some(self.ctx.id), kind, text, None);
    }
}

fn capitalize(text: &str) -> SharedString {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()).into(),
        None => SharedString::default(),
    }
}

/// Opens the Commit-author dialog and runs `opts` again with the identity it returns (09 §3.11): a one-off
/// override, or the saved identity (then Git finds it itself).
pub(crate) fn ask_identity_and_commit(
    tab: &mut RepoTab,
    opts: CommitOptions,
    continue_after: bool,
    window: &mut Window,
    cx: &mut Context<RepoTab>,
) {
    let ctx = tab.ctx().clone();
    let retry_ctx = ctx.clone();
    crate::dialogs::author::open_commit_identity(
        window,
        cx,
        ctx,
        Rc::new(move |choice, window, cx| {
            let next = match choice {
                IdentityChoice::Override(identity) => CommitOptions {
                    identity_override: Some(identity),
                    ..opts.clone()
                },
                IdentityChoice::Saved => opts.clone(),
            };
            retry_ctx.with_tab(window, cx, move |tab, window, cx| {
                tab.commit_inner(next, continue_after, window, cx);
            });
        }),
    );
}
