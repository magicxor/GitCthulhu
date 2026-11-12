//! Network operations of the catalogue (spec 21 §2.11; owned by G2 after G0b). G0b implements the reference
//! methods `fetch_all` (unguarded, cancellable) and `push_current` (unguarded; network marker on success and on the
//! three rejection errors; "Retry without hooks").

use gpui_kit::{Context, Window};

use super::{Cancellable, OpKind, OpSpec};
use crate::backend::oplog::push_marker;
use crate::backend::{
    ByteSlice as _, FetchOptions, FetchTarget, ForceMode, FullName, GitError, OperationKind,
    PullMode, PushReport, RefreshSet, TagFetch, TagPush, git,
};
use crate::tab::RepoTab;

/// The marker title of a push (17 §4.2): `Push {branch} to {remote}/{branch}` for one ref, else `Push to {remote}`.
pub(crate) fn push_title(report: &PushReport) -> String {
    let short = |name: &FullName| name.shorten().to_str_lossy().into_owned();
    match report.updates.as_slice() {
        [one] => {
            let local = one
                .local
                .as_ref()
                .map(short)
                .unwrap_or_else(|| short(&one.remote_ref));
            format!(
                "Push {local} to {}/{}",
                report.remote,
                short(&one.remote_ref)
            )
        }
        _ => format!("Push to {}", report.remote),
    }
}

/// The report of a successful push or of one of the three rejections (they carry the full report).
fn report_of<'a>(result: Result<&'a PushReport, &'a GitError>) -> Option<&'a PushReport> {
    match result {
        Ok(report) => Some(report),
        Err(
            GitError::NonFastForward { report, .. }
            | GitError::LeaseRejected { report, .. }
            | GitError::RemoteRejected { report, .. },
        ) => Some(report),
        Err(_) => None,
    }
}

impl RepoTab {
    /// ★ Fetches all remotes with pruning (`network::fetch`). Unguarded (20 §1.5), cancellable.
    /// Refresh: REMOTES | LOG | BRANCHES | TAGS.
    pub fn fetch_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let spec = OpSpec::new(OpKind::Fetch, |env| {
            git::network::fetch(
                env.repo,
                env.ctx,
                &FetchOptions {
                    target: FetchTarget::All,
                    prune: Some(true),
                    tags: TagFetch::default(),
                    refspecs: Vec::new(),
                },
            )
        })
        .unguarded()
        .cancellable(Cancellable::Yes)
        .label("Fetch all")
        .base_refresh(
            RefreshSet::REMOTES | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::TAGS,
        );
        self.run_op(spec, window, cx)
    }

    /// ★ Gitnuro's Push button: the current branch to its upstream (`network::push_current_branch`). `force` uses
    /// `--force-with-lease` when "push with lease" is on, else `--force`; `tags` pushes all tags. Unguarded; the
    /// pushed refs are recorded as a network marker (also after a partial rejection). Refresh: REMOTES | BRANCHES
    /// | LOG (| TAGS).
    pub fn push_current(
        &mut self,
        force: bool,
        tags: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.push_current_with(force, tags, false, window, cx)
    }

    fn push_current_with(
        &mut self,
        force: bool,
        tags: bool,
        skip_hooks: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let force_mode = match (force, crate::settings::get_or_default(cx).push_with_lease) {
            (false, _) => ForceMode::No,
            (true, true) => ForceMode::WithLease,
            (true, false) => ForceMode::Force,
        };
        let tag_push = if tags { TagPush::All } else { TagPush::None };
        let marker_kind = if tags {
            OperationKind::PushTags
        } else {
            OperationKind::Push
        };
        let mut refresh = RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG;
        if tags {
            refresh |= RefreshSet::TAGS;
        }
        let spec = OpSpec::new(OpKind::Push, move |env| {
            git::network::push_current_branch(env.repo, env.ctx, force_mode, tag_push, skip_hooks)
        })
        .unguarded()
        .marker(move |result| {
            let report = report_of(result)?;
            push_marker(marker_kind, push_title(report), report)
        })
        .cancellable(Cancellable::Yes)
        .label("Push")
        .base_refresh(refresh)
        .retry_without_hooks(move |tab, window, cx| {
            tab.push_current_with(force, tags, true, window, cx);
        });
        self.run_op(spec, window, cx)
    }

    /// Fetches one remote. Owner: G2.
    pub fn fetch_remote(
        &mut self,
        _name: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Fetch", cx);
        false
    }

    /// Pulls the current branch (`None` = the mode from `pull_with_rebase`). Owner: G2.
    pub fn pull(
        &mut self,
        _mode: Option<PullMode>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Pull", cx);
        false
    }

    /// Pulls from a specific remote branch. Owner: G2.
    pub fn pull_from(
        &mut self,
        _remote_branch: FullName,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Pull", cx);
        false
    }

    /// Pushes `local` to `remote_branch` (`network::push`). Owner: G2.
    pub fn push_branch_to(
        &mut self,
        _local: FullName,
        _remote_branch: FullName,
        _force: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Push", cx);
        false
    }

    /// Deletes a branch on a remote. Owner: G2.
    pub fn delete_remote_branch(
        &mut self,
        _remote: String,
        _branch: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete remote branch", cx);
        false
    }

    /// Pushes one tag. Owner: G2.
    pub fn push_tag(
        &mut self,
        _remote: String,
        _tag: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Push tag", cx);
        false
    }

    /// Deletes a tag on a remote. Owner: G2.
    pub fn delete_remote_tag(
        &mut self,
        _remote: String,
        _tag: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete remote tag", cx);
        false
    }
}
