//! Commit operations of the catalogue (spec 21 §2.11; owned by G5 after G0b). G0b implements the reference method
//! `commit` (snapshot record, cancellable with warning, "Retry without hooks").

use gpui_kit::{Context, Window};

use super::{Cancellable, OpKind, OpSpec};
use crate::backend::UntrackedCapture;
use crate::backend::{CommitOptions, OperationDescriptor, OperationKind, RefreshSet, Scope, git};
use crate::tab::RepoTab;

/// The summary line of a commit message (oplog titles).
fn summary(message: &str) -> &str {
    message.lines().next().unwrap_or_default().trim()
}

impl RepoTab {
    /// ★ `git commit` (hooks included): `Commit "{summary}"` / `Amend "{summary}"` snapshot, cancellable with a
    /// warning, "Retry without hooks" after a failing pre hook. Refresh: STATUS | LOG | BRANCHES | REPO_STATE.
    pub fn commit(
        &mut self,
        opts: CommitOptions,
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
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::commit::commit(repo, env.perm(), ctx, &opts)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(
            RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::REPO_STATE,
        )
        .retry_without_hooks(move |tab, window, cx| {
            tab.commit(retry_opts, window, cx);
        });
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

    /// `--continue` of the operation in progress (`merge::continue_operation`). Owner: G5.
    pub fn continue_operation(
        &mut self,
        _message: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Continue", cx);
        false
    }

    /// `--abort` of the operation in progress (`merge::abort_operation`). Owner: G5.
    pub fn abort_operation(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Abort", cx);
        false
    }

    /// `--skip` of the operation in progress (`merge::skip_operation`). Owner: G5.
    pub fn skip_operation(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Skip", cx);
        false
    }
}
