//! Applies the undo settings (`record_undo_history`, `undo_keep_entries`, `undo_keep_days`,
//! `undo_capture_untracked`) to the tab's oplog (spec 21 §2.5, 17 §4.4 last row).
//!
//! Every open tab observes the settings: when a key changes it calls `apply_undo_settings(old_enabled, new)`. While an
//! operation runs the change is queued and applied when it finishes; otherwise a background closure runs
//! `set_oplog_options` and, when recording was turned back on, records a barrier under the guard, then reloads
//! UNDO. Applications run one at a time per tab: a change that arrives while one is in flight is queued too, so a
//! burst of edits (typing a number) can never leave an older value in the oplog. A guard timeout retries the barrier
//! once after the next operation.

use gpui_kit::{AppContext as _, Context, Subscription, Task};

use super::RepoTab;
use super::ops::LOCK_TIMEOUT;
use crate::backend::RefreshSet;
use crate::backend::handle::{TabBackend, UndoSettings};
use crate::settings::SettingsGlobal;

/// Barrier title and reason (17 §4.4).
const BARRIER_TITLE: &str = "Record undo history";
const BARRIER_REASON: &str = "Undo history was paused";

/// Undo-settings state of a tab.
pub struct UndoState {
    /// The settings last applied (or being applied).
    pub(crate) applied: UndoSettings,
    /// A change that waits for the running operation or the running application: (recording was enabled before,
    /// new settings).
    pub(crate) queued: Option<(bool, UndoSettings)>,
    /// The barrier was already retried once after a guard timeout (no second retry).
    pub(crate) retry: bool,
    /// The barrier hit a guard timeout: record it after the next operation.
    pub(crate) retry_barrier: bool,
    task: Option<Task<()>>,
}

impl UndoState {
    /// The state of a tab opened with `applied`.
    pub fn new(applied: UndoSettings) -> Self {
        Self {
            applied,
            queued: None,
            retry: false,
            retry_barrier: false,
            task: None,
        }
    }

    /// An application runs in the background.
    pub fn is_applying(&self) -> bool {
        self.task.is_some()
    }
}

/// Subscribes the tab to settings changes.
pub(crate) fn observe(cx: &mut Context<RepoTab>) -> Subscription {
    cx.observe_global::<SettingsGlobal>(|tab, cx| {
        let new = UndoSettings::from_settings(&crate::settings::get_or_default(cx));
        let old = tab.undo.applied;
        if new != old {
            tab.apply_undo_settings(old.enabled, new, cx);
        }
    })
}

impl RepoTab {
    /// Applies `new` to this tab's oplog (queued while an operation runs). `old_enabled` = recording was on before;
    /// turning it back on records a barrier.
    pub fn apply_undo_settings(
        &mut self,
        old_enabled: bool,
        new: UndoSettings,
        cx: &mut Context<Self>,
    ) {
        self.undo.applied = new;
        // One application at a time: two background `set_oplog_options` calls could finish in either order.
        if self.busy.is_some() || self.undo.task.is_some() {
            let first_old = self
                .undo
                .queued
                .map_or(old_enabled, |(queued_old, _)| queued_old);
            self.undo.queued = Some((first_old, new));
            return;
        }
        let barrier = !old_enabled && new.enabled;
        match self.ctx.backend.clone() {
            TabBackend::Real(handle) => {
                let bg = cx.background_spawn(async move {
                    handle.set_oplog_options(&new);
                    if !barrier {
                        return Ok(());
                    }
                    let Some(mut guard) = handle.try_lock(LOCK_TIMEOUT) else {
                        return Err(None);
                    };
                    handle
                        .oplog_barrier(
                            BARRIER_TITLE.to_owned(),
                            BARRIER_REASON.to_owned(),
                            guard.write_permission(),
                        )
                        .map_err(Some)
                });
                self.undo.task = Some(cx.spawn(async move |this, cx| {
                    let result = bg.await;
                    this.update(cx, |tab, cx| {
                        tab.undo.task = None;
                        match result {
                            Ok(()) => tab.undo.retry = false,
                            Err(None) => {
                                // Guard timeout: retry once after the next operation.
                                if !tab.undo.retry {
                                    tab.undo.retry = true;
                                    tab.undo.retry_barrier = true;
                                }
                            }
                            Err(Some(text)) => {
                                tracing::warn!("cannot record the undo barrier: {text}");
                                tab.undo.retry = false;
                            }
                        }
                        tab.reload(RefreshSet::UNDO, cx);
                        // A change that arrived meanwhile.
                        tab.apply_queued_undo_settings(cx);
                    })
                    .ok();
                }));
            }
            #[cfg(test)]
            TabBackend::Inert(inert) => {
                inert.record(crate::test_support::TestCall::Op {
                    kind: super::ops::OpKind::ApplyUndoSettings,
                    title: None,
                });
            }
        }
    }

    /// Applies a change queued while an operation or an earlier application ran.
    pub(crate) fn apply_queued_undo_settings(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.undo.task.is_some() {
            return;
        }
        if let Some((old_enabled, new)) = self.undo.queued.take() {
            self.apply_undo_settings(old_enabled, new, cx);
        }
    }

    /// After an operation: a barrier that hit a guard timeout is retried (once), then queued changes are applied.
    pub(crate) fn after_op_undo_settings(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.undo.retry_barrier) {
            // `old_enabled = false` makes the application record the barrier if recording is (still) on.
            let new = self
                .undo
                .queued
                .map_or(self.undo.applied, |(_, queued)| queued);
            self.undo.queued = Some((false, new));
        }
        self.apply_queued_undo_settings(cx);
    }

    /// The undo settings this tab applied last.
    pub fn applied_undo_settings(&self) -> UndoSettings {
        self.undo.applied
    }
}
