//! History operations of the catalogue (spec 21 §2.11; owned by G4 after G0b): merge, rebase, interactive rebase,
//! cherry-pick, revert and reset, with their operation-specific flows (21 §2.9): a merge commit to pick or revert
//! asks for the mainline parent first, an empty pick offers Skip / Commit empty / Leave as is, a merge whose
//! autostash conflicted and a rebase that stopped show Warning toasts.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, Context, SharedString, WeakEntity, Window};

use super::{Cancellable, Handled, OpKind, OpSpec};
use crate::backend::present::{self, OpFailure};
use crate::backend::{
    ByteSlice as _, CommitOptions, FastForward, GitError, HeadState, MergeOptions, MergeOutcome,
    ObjectId, OpWarning, OperationDescriptor, OperationKind, PickOptions, RebaseOptions,
    RebaseOutcome, RebasePlan, RebaseStopReason, RefreshSet, ResetMode, Scope, UntrackedCapture,
    git,
};
use crate::dialogs::choice::{self, ChoiceResult, ChoiceSpec};
use crate::notifications::ToastKind;
use crate::tab::RepoTab;
use crate::tab::ui::RepoUiEvent;

/// Shown with the Warning toast of a merge whose autostash could not be re-applied cleanly.
const AUTOSTASH_DETAIL: &str = "Your local changes were stashed before the merge. Re-applying them after the \
merge produced conflicts: resolve the conflict markers in the working tree. The changes are kept in the stash list.";

/// `refs/heads/x`, `refs/remotes/o/x`, `refs/tags/x` without the namespace.
pub(crate) fn short_rev(rev: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|prefix| rev.strip_prefix(prefix))
        .unwrap_or(rev)
}

fn short_id(id: ObjectId) -> String {
    id.to_hex_with_len(7).to_string()
}

/// Cherry-pick or revert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    CherryPick,
    Revert,
}

impl Pick {
    fn noun(self) -> &'static str {
        match self {
            Pick::CherryPick => "cherry-pick",
            Pick::Revert => "revert",
        }
    }
}

/// Set by the background half when the outcome carries a warning the runner shows (post-hook failure, autostash
/// conflict): that Warning toast replaces the success toast, so `then` must not add a second, Positive one.
#[derive(Clone, Default)]
struct Warned(Arc<AtomicBool>);

impl Warned {
    fn note(&self, kind: OpKind, warnings: &[OpWarning]) {
        if present::post_hook_warning(kind, warnings).is_some() {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// What the log knows about a commit.
struct KnownCommit {
    parents: u8,
    summary: SharedString,
}

impl RepoTab {
    /// HEAD for oplog titles: the branch's short name, the short id when detached.
    fn head_label(&self, cx: &App) -> String {
        let model = self.ctx.model.read(cx);
        let head = model
            .state
            .value
            .as_ref()
            .map(|s| &s.head)
            .or_else(|| model.refs.value.as_ref().map(|r| &r.head));
        match head {
            Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) => {
                name.shorten().to_str_lossy().into_owned()
            }
            Some(HeadState::Detached { target }) => short_id(*target),
            None => "HEAD".to_owned(),
        }
    }

    /// The parent count and summary of a loaded commit.
    fn known_commit(&self, id: ObjectId, cx: &App) -> Option<KnownCommit> {
        let model = self.ctx.model.read(cx);
        let snapshot = model.log.value.as_ref()?;
        snapshot.rows.iter().find_map(|row| {
            let c = row.commit.as_ref().filter(|c| c.id == id)?;
            Some(KnownCommit {
                parents: c.parent_count,
                summary: c.summary.clone(),
            })
        })
    }

    /// A Positive toast after success with the "Undo" action of a recorded operation.
    fn recorded_toast(&self, kind: OpKind, text: impl Into<SharedString>, cx: &mut App) {
        let action =
            crate::undo::toast_action(&self.ctx, crate::undo::ToastContext::Recorded { kind });
        crate::app_view::show_toast(cx, Some(self.ctx.id), ToastKind::Positive, text, action);
    }

    /// The toast of a rebase step result: success, or a Warning naming why it stopped.
    fn rebase_toast(&self, kind: OpKind, outcome: &RebaseOutcome, warned: bool, cx: &mut App) {
        match outcome {
            // The runner already showed the warning toast that replaces the success toast.
            RebaseOutcome::Completed { .. } if warned => {}
            RebaseOutcome::Completed { .. } => {
                let text = kind.success_toast().unwrap_or("Rebase completed");
                self.recorded_toast(kind, text, cx);
            }
            RebaseOutcome::Stopped { reason, .. } => {
                let what = kind.operation_name();
                let text = match reason {
                    RebaseStopReason::Conflict => format!("{what} stopped with conflicts"),
                    RebaseStopReason::Edit => format!("{what} stopped for editing a commit"),
                    RebaseStopReason::Break => format!("{what} stopped at a break"),
                    RebaseStopReason::ExecFailed => format!("{what} stopped: an exec step failed"),
                    RebaseStopReason::ResolvedNeedsContinue => {
                        format!("{what} stopped: continue to apply the resolved commit")
                    }
                    RebaseStopReason::Unknown => format!("{what} stopped"),
                };
                crate::app_view::show_toast(cx, Some(self.ctx.id), ToastKind::Warning, text, None);
            }
        }
    }

    /// Merges `rev` into the current branch (`merge::merge`): fast-forward per "Fast-forward merge", `--autostash`
    /// per "Auto stash on merge". `Merge '{rev}' into {head}` snapshot, cancellable with a warning, "Retry without
    /// hooks". Refresh: OPERATION_STATE.
    pub fn merge(&mut self, rev: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let settings = crate::settings::get_or_default(cx);
        let opts = MergeOptions {
            rev,
            ff: if settings.fast_forward_merge {
                FastForward::Auto
            } else {
                FastForward::NoFf
            },
            squash: false,
            message: None,
            autostash: settings.auto_stash_on_merge,
            skip_hooks: false,
        };
        self.merge_with(opts, window, cx)
    }

    fn merge_with(
        &mut self,
        opts: MergeOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let descriptor = OperationDescriptor {
            kind: OperationKind::Merge,
            title: format!(
                "Merge '{}' into {}",
                short_rev(&opts.rev),
                self.head_label(cx)
            ),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let retry = MergeOptions {
            skip_hooks: true,
            ..opts.clone()
        };
        let warned = Warned::default();
        let note = warned.clone();
        let spec = OpSpec::new(OpKind::Merge, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let mut outcome = git::merge::merge(repo, env.perm(), ctx, &opts)?;
            if matches!(outcome.value, MergeOutcome::AutostashConflict { .. }) {
                outcome.warnings.push(OpWarning::AutostashConflict {
                    output: AUTOSTASH_DETAIL.to_owned(),
                });
            }
            note.note(OpKind::Merge, &outcome.warnings);
            Ok(outcome)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .toast("")
        .then(move |tab, outcome: &MergeOutcome, _, cx| match outcome {
            // The runner showed the warning toast instead of a success toast.
            _ if warned.get() => {}
            MergeOutcome::UpToDate => crate::app_view::show_toast(
                cx,
                Some(tab.ctx.id),
                ToastKind::Positive,
                "Already up to date",
                None,
            ),
            MergeOutcome::Squashed => {
                tab.recorded_toast(OpKind::Merge, "Merge squashed: the changes are staged", cx)
            }
            MergeOutcome::FastForwarded { .. } | MergeOutcome::Merged { .. } => {
                let text = OpKind::Merge.success_toast().unwrap_or("Merge completed");
                tab.recorded_toast(OpKind::Merge, text, cx);
            }
            // The runner shows the AutostashConflict warning toast.
            MergeOutcome::AutostashConflict { .. } => {}
        })
        .retry_without_hooks(move |tab, window, cx| {
            tab.merge_with(retry, window, cx);
        });
        self.run_op(spec, window, cx)
    }

    /// Rebases the current branch onto `onto_rev` (`rebase::rebase`). A stop (conflict, edit, ...) is a successful
    /// step that leaves the rebase in progress (Warning toast; the commit box offers Continue / Skip / Abort).
    /// `Rebase {head} onto '{rev}'` snapshot, cancellable with a warning, "Retry without hooks". Refresh:
    /// OPERATION_STATE.
    pub fn rebase(
        &mut self,
        onto_rev: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.rebase_with(onto_rev, RebaseOptions::default(), window, cx)
    }

    fn rebase_with(
        &mut self,
        onto_rev: String,
        opts: RebaseOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let descriptor = OperationDescriptor {
            kind: OperationKind::Rebase,
            title: format!(
                "Rebase {} onto '{}'",
                self.head_label(cx),
                short_rev(&onto_rev)
            ),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let (retry_rev, retry_opts) = (
            onto_rev.clone(),
            RebaseOptions {
                skip_hooks: true,
                ..opts
            },
        );
        let warned = Warned::default();
        let note = warned.clone();
        let spec = OpSpec::new(OpKind::Rebase, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let outcome = git::rebase::rebase(repo, env.perm(), ctx, &onto_rev, &opts)?;
            note.note(OpKind::Rebase, &outcome.warnings);
            Ok(outcome)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .toast("")
        .then(move |tab, outcome: &RebaseOutcome, _, cx| {
            tab.rebase_toast(OpKind::Rebase, outcome, warned.get(), cx)
        })
        .retry_without_hooks(move |tab, window, cx| {
            tab.rebase_with(retry_rev, retry_opts, window, cx);
        });
        self.run_op(spec, window, cx)
    }

    /// Starts an interactive rebase with the edited plan (`rebase::start_interactive`); the editor closes once the
    /// rebase ran (completed or stopped). `Interactive rebase of {n} commits` snapshot, cancellable with a warning,
    /// "Retry without hooks". Refresh: OPERATION_STATE.
    pub fn start_interactive_rebase(
        &mut self,
        plan: RebasePlan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.start_interactive_with(plan, RebaseOptions::default(), window, cx)
    }

    fn start_interactive_with(
        &mut self,
        plan: RebasePlan,
        opts: RebaseOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let n = plan.steps.len();
        let descriptor = OperationDescriptor {
            kind: OperationKind::RebaseInteractive,
            title: format!(
                "Interactive rebase of {n} commit{}",
                if n == 1 { "" } else { "s" }
            ),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let (retry_plan, retry_opts) = (
            plan.clone(),
            RebaseOptions {
                skip_hooks: true,
                ..opts
            },
        );
        let warned = Warned::default();
        let note = warned.clone();
        let spec = OpSpec::new(OpKind::RebaseInteractive, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let outcome = git::rebase::start_interactive(repo, env.perm(), ctx, &plan, &opts)?;
            note.note(OpKind::RebaseInteractive, &outcome.warnings);
            Ok(outcome)
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .toast("")
        .then(move |tab, outcome: &RebaseOutcome, _, cx| {
            tab.ctx.ui.update(cx, |ui, cx| {
                if ui.rebase_plan.take().is_some() {
                    cx.emit(RepoUiEvent::CenterChanged);
                    cx.notify();
                }
            });
            tab.rebase_toast(OpKind::RebaseInteractive, outcome, warned.get(), cx);
        })
        .retry_without_hooks(move |tab, window, cx| {
            tab.start_interactive_with(retry_plan, retry_opts, window, cx);
        });
        self.run_op(spec, window, cx)
    }

    /// Cherry-picks a commit (`merge::cherry_pick`). A merge commit without `mainline` asks for the parent first
    /// (returns `false` then); an empty result offers Skip / Commit empty / Leave as is. `Cherry-pick {short id}`
    /// snapshot, cancellable with a warning. Refresh: OPERATION_STATE.
    pub fn cherry_pick(
        &mut self,
        id: ObjectId,
        mainline: Option<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.pick(Pick::CherryPick, id, mainline, window, cx)
    }

    /// Reverts a commit (`merge::revert`); flows as [`Self::cherry_pick`]. `Revert {short id}` snapshot.
    pub fn revert(
        &mut self,
        id: ObjectId,
        mainline: Option<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.pick(Pick::Revert, id, mainline, window, cx)
    }

    fn pick(
        &mut self,
        pick: Pick,
        id: ObjectId,
        mainline: Option<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let known = self.known_commit(id, cx);
        let summary = known.as_ref().map(|k| k.summary.clone());
        if mainline.is_none()
            && let Some(k) = &known
            && k.parents > 1
        {
            self.ask_mainline(pick, id, k.parents, summary, window, cx);
            return false;
        }
        let (kind, oplog_kind, verb) = match pick {
            Pick::CherryPick => (
                OpKind::CherryPickCommit,
                OperationKind::CherryPick,
                "Cherry-pick",
            ),
            Pick::Revert => (OpKind::RevertCommit, OperationKind::Revert, "Revert"),
        };
        let descriptor = OperationDescriptor {
            kind: oplog_kind,
            title: format!("{verb} {}", short_id(id)),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let opts = PickOptions {
            mainline,
            no_commit: false,
        };
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            match pick {
                Pick::CherryPick => git::merge::cherry_pick(repo, env.perm(), ctx, &[id], opts),
                Pick::Revert => git::merge::revert(repo, env.perm(), ctx, &[id], opts),
            }
        })
        .record(descriptor)
        .cancellable(Cancellable::WithWarning)
        .base_refresh(RefreshSet::OPERATION_STATE)
        .on_error(move |tab, failure, window, cx| {
            let OpFailure::Git(err) = failure else {
                return Handled::No;
            };
            match err.as_ref() {
                GitError::EmptyPick { .. } => {
                    tab.offer_empty_pick(pick, id, summary, window, cx);
                    Handled::Yes
                }
                GitError::Unsupported { what } if what.contains("mainline") => {
                    tab.ask_mainline(pick, id, 2, summary, window, cx);
                    Handled::Yes
                }
                _ => Handled::No,
            }
        });
        self.run_op(spec, window, cx)
    }

    /// "Which parent is the mainline?" [Parent 1] [Parent 2] ... → the pick with `mainline = Some(n)`.
    fn ask_mainline(
        &mut self,
        pick: Pick,
        id: ObjectId,
        parents: u8,
        summary: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = match &summary {
            Some(s) => format!("{} '{s}'", short_id(id)),
            None => short_id(id),
        };
        let spec = ChoiceSpec {
            title: "Which parent is the mainline?".into(),
            message: format!(
                "{what} is a merge commit. Choose the parent the {} is relative to (parent 1 is the branch \
                 that was merged into).",
                pick.noun()
            )
            .into(),
            options: (1..=parents.max(2))
                .map(|n| SharedString::from(format!("Parent {n}")))
                .collect(),
            checkbox: None,
        };
        let tab = cx.weak_entity();
        choice::open(
            window,
            cx,
            spec,
            Box::new(move |result: ChoiceResult, window, cx| {
                let Some(option) = result.option else {
                    return;
                };
                let mainline = u8::try_from(option + 1).ok();
                with_tab(&tab, window, cx, move |tab, window, cx| {
                    tab.pick(pick, id, mainline, window, cx);
                });
            }),
        );
    }

    /// "The cherry-pick resulted in no changes": [Skip] → `skip_operation`, [Commit empty] → an empty commit with
    /// the operation's message, [Leave as is] keeps the operation in progress.
    fn offer_empty_pick(
        &mut self,
        pick: Pick,
        id: ObjectId,
        summary: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = match &summary {
            Some(s) => format!("{} '{s}'", short_id(id)),
            None => short_id(id),
        };
        let spec = ChoiceSpec {
            title: format!("The {} resulted in no changes", pick.noun()).into(),
            message: format!(
                "The changes of {what} are already in the current branch. Skip the commit, record an empty \
                 commit, or leave the {} in progress.",
                pick.noun()
            )
            .into(),
            options: vec!["Skip".into(), "Commit empty".into(), "Leave as is".into()],
            checkbox: None,
        };
        let tab = cx.weak_entity();
        let fallback: Rc<str> = match (&summary, pick) {
            (Some(s), Pick::CherryPick) => s.as_ref().into(),
            (Some(s), Pick::Revert) => format!("Revert \"{s}\"").into(),
            (None, Pick::CherryPick) => format!("Cherry-pick {}", short_id(id)).into(),
            (None, Pick::Revert) => format!("Revert {}", short_id(id)).into(),
        };
        choice::open(
            window,
            cx,
            spec,
            Box::new(
                move |result: ChoiceResult, window, cx| match result.option {
                    Some(0) => with_tab(&tab, window, cx, |tab, window, cx| {
                        tab.skip_operation(window, cx);
                    }),
                    Some(1) => with_tab(&tab, window, cx, move |tab, window, cx| {
                        // MERGE_MSG (the draft while the pick is in progress) is Git's message of the pick.
                        let message = tab
                            .ctx
                            .model
                            .read(cx)
                            .draft_message
                            .clone()
                            .filter(|m| !m.trim().is_empty())
                            .unwrap_or_else(|| fallback.to_string());
                        tab.commit_with(
                            CommitOptions {
                                message,
                                allow_empty: true,
                                ..CommitOptions::default()
                            },
                            window,
                            cx,
                        );
                    }),
                    _ => {}
                },
            ),
        );
    }

    /// Resets the current branch to a commit (`merge::reset`). The Reset dialog chose the mode and confirmed a hard
    /// reset. `Reset {head} to {short id} ({mode})` snapshot (`--keep` is recorded as a hard reset, titled
    /// "Reset (keep) ..."). Refresh: OPERATION_STATE.
    pub fn reset(
        &mut self,
        id: ObjectId,
        mode: ResetMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let head = self.head_label(cx);
        let (oplog_kind, title) = match mode {
            ResetMode::Soft => (
                OperationKind::ResetSoft,
                format!("Reset {head} to {} (soft)", short_id(id)),
            ),
            ResetMode::Mixed => (
                OperationKind::ResetMixed,
                format!("Reset {head} to {} (mixed)", short_id(id)),
            ),
            ResetMode::Hard => (
                OperationKind::ResetHard,
                format!("Reset {head} to {} (hard)", short_id(id)),
            ),
            ResetMode::Keep => (
                OperationKind::ResetHard,
                format!("Reset (keep) {head} to {}", short_id(id)),
            ),
        };
        let descriptor = OperationDescriptor {
            kind: oplog_kind,
            title,
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::ResetToCommit, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::merge::reset(repo, env.perm(), ctx, id, mode)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::OPERATION_STATE);
        self.run_op(spec, window, cx)
    }
}

/// Runs `f` on the tab behind `tab` from a dialog callback (outside the tab's update).
fn with_tab(
    tab: &WeakEntity<RepoTab>,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>),
) {
    if let Some(tab) = tab.upgrade() {
        tab.update(cx, |tab, cx| f(tab, window, cx));
    }
}
