//! Commit operations of the catalogue (spec 21 §2.11; owned by G5): `git commit` (also amend and the concluding
//! commit of a merge, cherry-pick or revert), and continue / abort / skip of the operation in progress.

use std::rc::Rc;

use gpui_kit::{App, Context, SharedString, Window};

use super::{Cancellable, Handled, OpKind, OpSpec};
use crate::app_view::show_toast;
use crate::backend::UntrackedCapture;
use crate::backend::{
    CommitOptions, ContinueOutcome, GitErrorKind, Identity, InProgress, OperationDescriptor,
    OperationKind, RebaseOutcome, RebaseStopReason, RefreshSet, Scope, git,
};
use crate::dialogs::author::IdentityChoice;
use crate::notifications::ToastKind;
use crate::settings::Signoff;
use crate::tab::{RepoTab, TabCtx};

/// What a commit needs to build its sign-off again: the options as the user gave them (before any sign-off) and the
/// repository's sign-off setting. A retry with another identity (Git had none) rebuilds the trailer from the
/// identity that was actually chosen, never from the one the failed attempt had.
#[derive(Clone, Debug)]
pub(crate) struct Resign {
    raw: CommitOptions,
    signoff: Signoff,
}

/// The identity Git would commit with, as far as the model knows it.
fn effective_identity(ctx: &TabCtx, cx: &App) -> Option<Identity> {
    ctx.model
        .read(cx)
        .identity
        .value
        .as_ref()
        .and_then(|info| info.effective.clone())
}

/// Applies the repository's sign-off setting `pref`: Git's own `--signoff` for the default text, otherwise the
/// configured text (with `%user` and `%email` replaced by `identity`) as a trailer.
pub(crate) fn apply_signoff(pref: &Signoff, opts: &mut CommitOptions, identity: Option<Identity>) {
    if !pref.enabled {
        return;
    }
    let text = pref.text.trim();
    if text.is_empty() {
        opts.signoff = true;
        return;
    }
    let (name, email) = identity.map(|i| (i.name, i.email)).unwrap_or_default();
    let text = text.replace("%user", &name).replace("%email", &email);
    match text.split_once(':') {
        Some((key, value)) => opts
            .trailers
            .push((key.trim().to_owned(), value.trim().to_owned())),
        None => {
            opts.message.push_str("\n\n");
            opts.message.push_str(&text);
        }
    }
}

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
        self.commit_inner(opts, None, false, window, cx)
    }

    /// The commit box's commit: `raw` as typed plus the repository's sign-off setting. The sign-off text uses
    /// `chosen` (the identity the user just picked) or else the override or the identity Git has. With
    /// `continue_after` the commit amends HEAD at a rebase `edit` stop and then continues the rebase (05 §3.3: the
    /// GUI commits first, then calls `--continue`). When Git reports an unknown identity, the retry builds the
    /// sign-off again from the identity chosen in the dialog.
    pub(crate) fn commit_signed(
        &mut self,
        raw: CommitOptions,
        signoff: Signoff,
        chosen: Option<Identity>,
        continue_after: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut opts = raw.clone();
        let identity = opts
            .identity_override
            .clone()
            .or(chosen)
            .or_else(|| effective_identity(&self.ctx, cx));
        apply_signoff(&signoff, &mut opts, identity);
        self.commit_inner(
            opts,
            Some(Resign { raw, signoff }),
            continue_after,
            window,
            cx,
        )
    }

    fn commit_inner(
        &mut self,
        opts: CommitOptions,
        resign: Option<Resign>,
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
        let identity_resign = resign.clone();
        let retry_resign = resign.map(|mut resign| {
            resign.raw.skip_hooks = true;
            resign
        });
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
            ask_identity_and_commit(
                tab,
                identity_opts,
                identity_resign.clone(),
                continue_after,
                window,
                cx,
            );
            Handled::Yes
        })
        .retry_without_hooks(move |tab, window, cx| {
            tab.commit_inner(retry_opts, retry_resign.clone(), continue_after, window, cx);
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
    resign: Option<Resign>,
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
            match resign.clone() {
                // The sign-off is built again from the identity that was chosen.
                Some(Resign { mut raw, signoff }) => {
                    if let IdentityChoice::Override(identity) = &choice {
                        raw.identity_override = Some(identity.clone());
                    }
                    let chosen = choice.identity().clone();
                    retry_ctx.with_tab(window, cx, move |tab, window, cx| {
                        tab.commit_signed(raw, signoff, Some(chosen), continue_after, window, cx);
                    });
                }
                None => {
                    let next = match choice {
                        IdentityChoice::Override(identity) => CommitOptions {
                            identity_override: Some(identity),
                            ..opts.clone()
                        },
                        IdentityChoice::Saved(_) => opts.clone(),
                    };
                    retry_ctx.with_tab(window, cx, move |tab, window, cx| {
                        tab.commit_inner(next, None, continue_after, window, cx);
                    });
                }
            }
        }),
    );
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn pref(text: &str) -> Signoff {
        Signoff {
            enabled: true,
            text: text.to_owned(),
        }
    }

    fn eve() -> Identity {
        Identity {
            name: "Eve".into(),
            email: "eve@example.com".into(),
        }
    }

    #[test]
    fn a_custom_signoff_names_the_identity_it_is_given() {
        let mut opts = CommitOptions {
            message: "msg".into(),
            ..CommitOptions::default()
        };
        apply_signoff(
            &pref("Signed-off-by: %user <%email>"),
            &mut opts,
            Some(eve()),
        );
        assert_eq!(
            opts.trailers,
            [(
                "Signed-off-by".to_owned(),
                "Eve <eve@example.com>".to_owned()
            )]
        );
        assert!(!opts.signoff, "a custom text is a trailer, not --signoff");
    }

    #[test]
    fn rebuilding_from_the_raw_options_replaces_the_trailer_of_the_failed_identity() {
        // The failed attempt had no identity: its trailer is empty. The retry starts again from the raw options,
        // so nothing of the first trailer survives.
        let raw = CommitOptions {
            message: "msg".into(),
            ..CommitOptions::default()
        };
        let mut failed = raw.clone();
        apply_signoff(&pref("Signed-off-by: %user <%email>"), &mut failed, None);
        assert_eq!(
            failed.trailers[0].1, "<>",
            "no identity: empty placeholders"
        );
        let mut retry = raw;
        apply_signoff(
            &pref("Signed-off-by: %user <%email>"),
            &mut retry,
            Some(eve()),
        );
        assert_eq!(retry.trailers.len(), 1);
        assert_eq!(retry.trailers[0].1, "Eve <eve@example.com>");
    }

    #[test]
    fn the_default_text_asks_git_for_its_own_signoff_and_a_disabled_pref_does_nothing() {
        let mut opts = CommitOptions::default();
        apply_signoff(&pref(""), &mut opts, None);
        assert!(opts.signoff);
        let mut opts = CommitOptions::default();
        apply_signoff(&Signoff::default(), &mut opts, Some(eve()));
        assert!(!opts.signoff && opts.trailers.is_empty());
    }

    #[test]
    fn text_without_a_colon_is_appended_to_the_message() {
        let mut opts = CommitOptions {
            message: "msg".into(),
            ..CommitOptions::default()
        };
        apply_signoff(&pref("Reviewed by %user"), &mut opts, Some(eve()));
        assert_eq!(opts.message, "msg\n\nReviewed by Eve");
    }
}
