//! Network operations of the catalogue (spec 21 §2.11, flows of §2.9; owned by G2).
//!
//! Fetch and the push family are unguarded (20 §1.5) and record a network marker instead of a snapshot; pull takes
//! the guard and records `Pull {head}`. The recovery flows live here: a divergent pull without a configured
//! strategy asks Merge or Rebase (and can remember the answer in the repository's `pull.rebase`), a pull or push of
//! a branch without an upstream opens the Change-upstream dialog and runs again once the upstream is set, and a
//! failing pre-push hook offers "Retry without hooks".

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{Context, Subscription, Window};

use super::{Cancellable, Handled, OpKind, OpSpec};
use crate::backend::loaders;
use crate::backend::oplog::push_marker;
use crate::backend::present::OpFailure;
use crate::backend::{
    ByteSlice as _, ConfigScope, FetchOptions, FetchReport, FetchTarget, ForceMode, FullName,
    GitError, GitErrorKind, HeadState, MergeOutcome, MergeOutcomeOrRebase, NetworkMarker,
    OperationDescriptor, OperationKind, PullMode, PullOptions, PushOptions, PushRefSpec,
    PushRefStatus, PushReport, RefList, RefreshSet, RemoteInfo, Scope, TagFetch, TagPush,
    UntrackedCapture, git,
};
use crate::dialogs::choice::{self, ChoiceResult, ChoiceSpec};
use crate::notifications::ToastKind;
use crate::tab::{RepoTab, RepoTabEvent};

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

/// The network marker of a push result (17 §4.4): the updates that happened on the remote, also after a partial
/// rejection. Refs that were already up to date changed nothing and are not recorded (a push of an unchanged branch
/// leaves no marker); the title names what changed (a branch pushed with `--tags` whose tags were all on the remote
/// already is `Push main to origin/main`, not `Push to origin`).
fn marker_of(
    kind: OperationKind,
    title: impl FnOnce(&PushReport) -> String,
    result: Result<&PushReport, &GitError>,
) -> Option<NetworkMarker> {
    let report = report_of(result)?;
    let mut changed = report.clone();
    changed.updates.retain(|update| {
        !matches!(
            update.status,
            PushRefStatus::UpToDate | PushRefStatus::Rejected
        )
    });
    let title = title(&changed);
    push_marker(kind, title, &changed)
}

/// Splits `refs/remotes/<remote>/<branch>` into the remote and the branch name. Known remote names win (a remote
/// name may contain `/`), else the first path segment is the remote.
fn split_remote_branch(remotes: &[RemoteInfo], full: &FullName) -> Option<(String, String)> {
    let text = full.as_bstr().to_str_lossy().into_owned();
    let rest = text.strip_prefix("refs/remotes/")?;
    let known = remotes
        .iter()
        .filter_map(|remote| {
            rest.strip_prefix(remote.name.as_str())
                .and_then(|r| r.strip_prefix('/'))
                .filter(|branch| !branch.is_empty())
                .map(|branch| (remote.name.clone(), branch.to_owned()))
        })
        .max_by_key(|(remote, _)| remote.len());
    known.or_else(|| {
        rest.split_once('/')
            .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
            .map(|(remote, branch)| (remote.to_owned(), branch.to_owned()))
    })
}

/// The warning of a fetch in which some remotes failed although the operation as a whole succeeded.
fn fetch_failure_text(report: &FetchReport) -> Option<String> {
    let failed: Vec<(&str, &str)> = report
        .per_remote
        .iter()
        .filter_map(|(remote, result)| {
            result.as_ref().err().map(|message| {
                (
                    remote.as_str(),
                    message.lines().next().unwrap_or_default().trim(),
                )
            })
        })
        .collect();
    match failed.as_slice() {
        [] => None,
        [(remote, message)] => Some(format!("Fetching {remote} failed: {message}")),
        many => Some(format!(
            "Fetching failed for {} remotes: {}",
            many.len(),
            many.iter()
                .map(|(remote, _)| *remote)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// What a pull was asked to do; kept so a recovery flow can run it again.
#[derive(Clone, Debug)]
struct PullRequest {
    /// `Some((remote, branch))` pulls that remote branch instead of the upstream.
    target: Option<(String, String)>,
    /// `None` = the "Pull with rebase as default" setting decides.
    mode: Option<PullMode>,
}

/// What to run once the Change-upstream dialog set an upstream.
type AfterUpstream = Box<dyn FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>)>;

impl RepoTab {
    /// ★ Fetches all remotes with pruning (`network::fetch`). Unguarded (20 §1.5), cancellable. A remote that
    /// failed while others succeeded is reported by a warning toast. Refresh: REMOTES | LOG | BRANCHES | TAGS.
    pub fn fetch_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.fetch_target(FetchTarget::All, "Fetch all".to_owned(), window, cx)
    }

    /// Fetches one remote (the remote context menu). Refresh as [`RepoTab::fetch_all`].
    pub fn fetch_remote(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let label = format!("Fetch {name}");
        self.fetch_target(FetchTarget::Remote(name), label, window, cx)
    }

    fn fetch_target(
        &mut self,
        target: FetchTarget,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::Fetch, move |env| {
            git::network::fetch(
                env.repo,
                env.ctx,
                &FetchOptions {
                    target,
                    prune: Some(true),
                    tags: TagFetch::default(),
                    refspecs: Vec::new(),
                },
            )
        })
        .unguarded()
        .cancellable(Cancellable::Yes)
        .label(label)
        .base_refresh(
            RefreshSet::REMOTES | RefreshSet::LOG | RefreshSet::BRANCHES | RefreshSet::TAGS,
        )
        .then(|tab, report: &FetchReport, _, cx| {
            if let Some(text) = fetch_failure_text(report) {
                crate::app_view::show_toast(cx, Some(tab.ctx.id), ToastKind::Warning, text, None);
            }
        });
        self.run_op(spec, window, cx)
    }

    /// Pulls the current branch from its upstream. `None` = Gitnuro's default (rebase iff "Pull with rebase as
    /// default"; otherwise Git's own configuration decides and a divergent pull asks Merge or Rebase). Snapshot
    /// `Pull {head}`; `--autostash` follows "Stash changes before merging". Refresh: OPERATION_STATE | REMOTES.
    pub fn pull(
        &mut self,
        mode: Option<PullMode>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.run_pull(PullRequest { target: None, mode }, window, cx)
    }

    /// Pulls a specific remote branch (`refs/remotes/<remote>/<branch>`) into the current branch.
    pub fn pull_from(
        &mut self,
        remote_branch: FullName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let target = self.remote_target(&remote_branch, cx);
        let Some(target) = target else {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                format!("'{}' is not a remote branch", remote_branch.shorten()),
                None,
            );
            return false;
        };
        self.run_pull(
            PullRequest {
                target: Some(target),
                mode: None,
            },
            window,
            cx,
        )
    }

    fn run_pull(
        &mut self,
        request: PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let settings = crate::settings::get_or_default(cx);
        let mode = request.mode.unwrap_or(if settings.pull_with_rebase {
            PullMode::Rebase
        } else {
            PullMode::FromConfig
        });
        let (remote, branch) = match &request.target {
            Some((remote, branch)) => (Some(remote.clone()), Some(branch.clone())),
            None => (None, None),
        };
        let options = PullOptions {
            remote,
            branch,
            mode,
            autostash: settings.auto_stash_on_merge,
        };
        let head = self.head_label(cx);
        let (title, detail, label) = match &request.target {
            Some((remote, branch)) => (
                format!("Pull {head}"),
                Some(format!("from {remote}/{branch}")),
                format!("Pull from {remote}"),
            ),
            None => (format!("Pull {head}"), None, "Pull".to_owned()),
        };
        let overlay = self.pull_overlay_text(&request, cx);
        let descriptor = OperationDescriptor {
            kind: OperationKind::Pull,
            title,
            detail,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let mut spec = OpSpec::new(OpKind::Pull, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let mut outcome = git::network::pull(repo, env.perm(), ctx, &options)?;
            // Like a merge: the runner shows ONE Warning toast (with "Show output") instead of the success toast.
            if matches!(
                outcome.value,
                MergeOutcomeOrRebase::Merge(MergeOutcome::AutostashConflict { .. })
            ) {
                outcome
                    .warnings
                    .push(crate::backend::OpWarning::AutostashConflict {
                        output: super::history::AUTOSTASH_DETAIL.to_owned(),
                    });
            }
            Ok(outcome)
        })
        .record(descriptor)
        .cancellable(Cancellable::Yes)
        .label(label)
        .base_refresh(RefreshSet::OPERATION_STATE | RefreshSet::REMOTES)
        .on_error(move |tab, failure, window, cx| tab.pull_failed(request, failure, window, cx));
        if let Some(text) = overlay {
            spec = spec.detail(text);
        }
        self.run_op(spec, window, cx)
    }

    /// The overlay subtitle of a pull that names its source: the requested remote branch, else the current
    /// branch's upstream, in Gitnuro's words (07 §12.6: "Pulling changes from origin/main to the current");
    /// `None` while neither is known (the generic subtitle then says "remote branch").
    fn pull_overlay_text(&self, request: &PullRequest, cx: &Context<Self>) -> Option<String> {
        let source = match &request.target {
            Some((remote, branch)) => format!("{remote}/{branch}"),
            None => {
                let model = self.ctx.model.read(cx);
                let refs = model.refs.value.as_ref()?;
                let HeadState::Branch { name, .. } = &refs.head else {
                    return None;
                };
                let upstream = refs
                    .local
                    .iter()
                    .find(|b| &b.full == name)?
                    .upstream
                    .as_ref()?;
                format!(
                    "{}/{}",
                    upstream.remote,
                    upstream.merge.as_ref().shorten().to_str_lossy()
                )
            }
        };
        Some(format!("Pulling changes from {source} to the current"))
    }

    /// The recovery flows of a failed pull (21 §2.9).
    fn pull_failed(
        &mut self,
        request: PullRequest,
        failure: &OpFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Handled {
        match failure.git_kind() {
            Some(GitErrorKind::PullStrategyRequired) => {
                self.ask_pull_strategy(request, window, cx);
                Handled::Yes
            }
            Some(GitErrorKind::NoUpstream) => self.ask_upstream(
                window,
                cx,
                Box::new(move |tab, window, cx| {
                    tab.run_pull(request, window, cx);
                }),
            ),
            _ => Handled::No,
        }
    }

    /// "Your branch and its upstream have diverged": Merge or Rebase, optionally remembered in `pull.rebase`.
    fn ask_pull_strategy(
        &mut self,
        request: PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let head = self.head_label(cx);
        let tab = cx.weak_entity();
        let spec = ChoiceSpec {
            title: "Your branch and its upstream have diverged".into(),
            message: format!(
                "Choose how to integrate the upstream changes into '{head}'. Git has no pull strategy configured for this repository."
            )
            .into(),
            options: vec!["Merge".into(), "Rebase".into()],
            checkbox: Some("Remember for this repository".into()),
        };
        choice::open(
            window,
            cx,
            spec,
            Box::new(move |answer: ChoiceResult, window, cx| {
                let mode = match answer.option {
                    Some(0) => PullMode::Merge,
                    Some(1) => PullMode::Rebase,
                    _ => return,
                };
                tab.update(cx, |tab, cx| {
                    tab.retry_pull_with(request, mode, answer.checked, window, cx)
                })
                .ok();
            }),
        );
    }

    /// Runs the pull again with the chosen mode; with `remember` the choice is first written to `pull.rebase`.
    fn retry_pull_with(
        &mut self,
        request: PullRequest,
        mode: PullMode,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request = PullRequest {
            mode: Some(mode),
            ..request
        };
        let repo = self.ctx.backend.repo().cloned().filter(|_| remember);
        let Some(repo) = repo else {
            self.run_pull(request, window, cx);
            return;
        };
        let value = if mode == PullMode::Rebase {
            "true"
        } else {
            "false"
        };
        let task = crate::backend::bg(cx, move || {
            loaders::config_set(&repo, ConfigScope::Local, "pull.rebase", Some(value))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |tab, window, cx| {
                match result {
                    Ok(refresh) => tab.reload(refresh, cx),
                    Err(err) => crate::app_view::show_toast(
                        cx,
                        Some(tab.ctx.id),
                        ToastKind::Warning,
                        format!("Could not remember the pull strategy: {err}"),
                        None,
                    ),
                }
                tab.run_pull(request, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The Change-upstream flow of a pull or push of a branch without an upstream (21 §2.9): opens the dialog for
    /// the current branch and runs `after` once an upstream was set. The wait ends when the dialog is closed
    /// without a choice or when another operation starts first.
    fn ask_upstream(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        after: AfterUpstream,
    ) -> Handled {
        let branch = {
            let model = self.ctx.model.read(cx);
            let head = model
                .refs
                .value
                .as_ref()
                .map(|refs| refs.head.clone())
                .or_else(|| model.state.value.as_ref().map(|state| state.head.clone()));
            match head {
                Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) => Some(name),
                _ => None,
            }
        };
        let Some(branch) = branch else {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                "HEAD is detached: check out a branch first",
                None,
            );
            return Handled::Yes;
        };
        let events = self.ctx.events.clone();
        let waiting: Rc<RefCell<Option<Subscription>>> = Rc::default();
        let after = Rc::new(RefCell::new(Some(after)));
        let subscription = {
            let waiting = waiting.clone();
            cx.subscribe_in(
                &events,
                window,
                move |tab, _, event: &RepoTabEvent, window, cx| match event {
                    RepoTabEvent::OpStarted {
                        kind: OpKind::ChangeUpstream,
                    } => {}
                    RepoTabEvent::OpStarted { .. } => {
                        waiting.borrow_mut().take();
                    }
                    RepoTabEvent::OpFinished {
                        kind: OpKind::ChangeUpstream,
                        ok,
                    } => {
                        waiting.borrow_mut().take();
                        let after = after.borrow_mut().take();
                        if let Some(after) = after.filter(|_| *ok) {
                            after(tab, window, cx);
                        }
                    }
                    _ => {}
                },
            )
        };
        *waiting.borrow_mut() = Some(subscription);
        let waiting_for_dialog = waiting;
        let ctx = self.ctx.clone();
        window.defer(cx, move |window, cx| {
            crate::dialogs::branch::open_change_upstream_then(
                window,
                cx,
                ctx,
                branch,
                // A cancelled dialog, or an accepted one whose operation was refused, ends the wait: a later,
                // unrelated upstream change must not retry the pull or push.
                Box::new(move |started| {
                    if !started {
                        waiting_for_dialog.borrow_mut().take();
                    }
                }),
            );
        });
        Handled::Yes
    }

    /// ★ Gitnuro's Push button: the current branch to its upstream (`network::push_current_branch`). `force` uses
    /// `--force-with-lease` when "push with lease" is on, else `--force`; `tags` pushes all tags. Unguarded; the
    /// pushed refs are recorded as a network marker (also after a partial rejection). A branch without an upstream
    /// that has several candidate remotes asks for one. Refresh: REMOTES | BRANCHES | LOG (| TAGS).
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
        let force_mode = self.force_mode(force, cx);
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
        .marker(move |result| marker_of(marker_kind, push_title, result))
        .cancellable(Cancellable::Yes)
        .label("Push")
        .base_refresh(refresh)
        .retry_without_hooks(move |tab, window, cx| {
            tab.push_current_with(force, tags, true, window, cx);
        })
        .on_error(move |tab, failure, window, cx| {
            if failure.git_kind() == Some(GitErrorKind::NoUpstream) {
                return tab.ask_upstream(
                    window,
                    cx,
                    Box::new(move |tab, window, cx| {
                        tab.push_current_with(force, tags, skip_hooks, window, cx);
                    }),
                );
            }
            Handled::No
        });
        self.run_op(spec, window, cx)
    }

    /// Pushes the local branch `local` to the remote branch `remote_branch` (`refs/remotes/<remote>/<branch>`),
    /// forced per "Push with lease". Unguarded, marker `Push`. Refresh: REMOTES | BRANCHES | LOG.
    pub fn push_branch_to(
        &mut self,
        local: FullName,
        remote_branch: FullName,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.push_branch_to_with(local, remote_branch, force, false, false, window, cx)
    }

    /// [`RepoTab::push_branch_to`] for a branch without an upstream: the push also makes `remote_branch` its
    /// upstream (`git push -u`), so the toolbar's Push works for it afterwards (Create pull request, "Push the branch
    /// first").
    pub fn publish_branch_to(
        &mut self,
        local: FullName,
        remote_branch: FullName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.push_branch_to_with(local, remote_branch, false, true, false, window, cx)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_branch_to_with(
        &mut self,
        local: FullName,
        remote_branch: FullName,
        force: bool,
        set_upstream: bool,
        skip_hooks: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((remote, branch)) = self.remote_target(&remote_branch, cx) else {
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                format!("'{}' is not a remote branch", remote_branch.shorten()),
                None,
            );
            return false;
        };
        let destination = match FullName::try_from(format!("refs/heads/{branch}")) {
            Ok(name) => name,
            Err(err) => {
                crate::app_view::show_toast(
                    cx,
                    Some(self.ctx.id),
                    ToastKind::Warning,
                    format!("'{branch}' is not a valid branch name: {err}"),
                    None,
                );
                return false;
            }
        };
        let force_mode = self.force_mode(force, cx);
        let options = PushOptions {
            remote: remote.clone(),
            refspecs: vec![PushRefSpec {
                src: Some(local.clone()),
                dst: destination,
                expected_remote: None,
            }],
            force: force_mode,
            tags: TagPush::None,
            set_upstream,
            push_options: Vec::new(),
            skip_hooks,
        };
        let spec = OpSpec::new(OpKind::Push, move |env| {
            git::network::push(env.repo, env.ctx, &options)
        })
        .unguarded()
        .marker(|result| marker_of(OperationKind::Push, push_title, result))
        .cancellable(Cancellable::Yes)
        .label(format!("Push to {remote}"))
        .base_refresh(RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG)
        .retry_without_hooks(move |tab, window, cx| {
            tab.push_branch_to_with(local, remote_branch, force, set_upstream, true, window, cx);
        });
        self.run_op(spec, window, cx)
    }

    /// Deletes a branch on a remote (`git push --delete`). Unguarded, marker `Delete {remote}/{branch}`.
    /// Refresh: REMOTES | LOG.
    pub fn delete_remote_branch(
        &mut self,
        remote: String,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Delete {remote}/{branch}");
        let label = format!("Push to {remote}");
        let spec = OpSpec::new(OpKind::DeleteRemoteBranch, move |env| {
            git::network::delete_remote_branch(env.repo, env.ctx, &remote, &branch)
        })
        .unguarded()
        .marker(move |result| marker_of(OperationKind::DeleteRemoteBranch, |_| title, result))
        .cancellable(Cancellable::Yes)
        .label(label)
        .base_refresh(RefreshSet::REMOTES | RefreshSet::LOG);
        self.run_op(spec, window, cx)
    }

    /// Pushes one tag. Unguarded, marker `Push tag '{tag}' to {remote}`. Refresh: REMOTES | TAGS.
    pub fn push_tag(
        &mut self,
        remote: String,
        tag: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Push tag '{tag}' to {remote}");
        let label = format!("Push to {remote}");
        let spec = OpSpec::new(OpKind::PushTag, move |env| {
            git::network::push_tag(env.repo, env.ctx, &remote, &tag)
        })
        .unguarded()
        .marker(move |result| marker_of(OperationKind::PushTags, |_| title, result))
        .cancellable(Cancellable::Yes)
        .label(label)
        .base_refresh(RefreshSet::REMOTES | RefreshSet::TAGS);
        self.run_op(spec, window, cx)
    }

    /// Deletes a tag on a remote. Unguarded, marker `Delete tag '{tag}' on {remote}`. Refresh: REMOTES | TAGS.
    pub fn delete_remote_tag(
        &mut self,
        remote: String,
        tag: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Delete tag '{tag}' on {remote}");
        let label = format!("Push to {remote}");
        let spec = OpSpec::new(OpKind::DeleteRemoteTag, move |env| {
            git::network::delete_remote_tag(env.repo, env.ctx, &remote, &tag)
        })
        .unguarded()
        .marker(move |result| marker_of(OperationKind::DeleteRemoteTag, |_| title, result))
        .cancellable(Cancellable::Yes)
        .label(label)
        .base_refresh(RefreshSet::REMOTES | RefreshSet::TAGS);
        self.run_op(spec, window, cx)
    }

    /// `--force-with-lease` when "Push with lease" is on, else `--force`; `No` when not forcing.
    fn force_mode(&self, force: bool, cx: &gpui_kit::App) -> ForceMode {
        match (force, crate::settings::get_or_default(cx).push_with_lease) {
            (false, _) => ForceMode::No,
            (true, true) => ForceMode::WithLease,
            (true, false) => ForceMode::Force,
        }
    }

    /// The remote and branch name of a remote-tracking ref, resolved against the loaded remotes.
    fn remote_target(&self, full: &FullName, cx: &gpui_kit::App) -> Option<(String, String)> {
        let model = self.ctx.model.read(cx);
        let refs: Option<&RefList> = model.refs.value.as_deref();
        split_remote_branch(refs.map_or(&[][..], |r| r.remotes.as_slice()), full)
    }
}

#[cfg(test)]
mod tests;
