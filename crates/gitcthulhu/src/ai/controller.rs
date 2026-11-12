//! The per-tab AI controller: generates a commit message for the staged changes and streams it into the commit box
//! (spec 21 §5.3; 16 A7, A9).
//!
//! One generation at a time per tab. While it runs the message field is read-only and the button says Stop. Stop,
//! Escape, a tab switch, closing the tab and starting a commit cancel it and keep the text received so far; a
//! failure restores the text the user had and shows an Error toast.

use std::collections::HashMap;

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, WeakEntity, Window};

use super::button::AiButton;
use super::hub::{self, Readiness};
use super::jobs::{self, AiJob};
use crate::app;
use crate::app_view::AppViewGlobal;
use crate::backend::CancelToken;
use crate::backend::ai::{self as backend_ai, AiFailure};
use crate::settings;
use crate::status_pane::commit_box::CommitBox;
use crate::tab::ops::OpKind;
use crate::tab::{RepoTabEvent, TabCtx, TabId};

/// The running generation of a tab.
struct Run {
    id: u64,
    job: AiJob,
    commit_box: WeakEntity<CommitBox>,
    /// The message the user had when the generation started (restored on failure).
    original: String,
    _escape: Subscription,
    _tab_switch: Option<Subscription>,
}

/// The per-tab AI controller.
pub struct AiController {
    ctx: TabCtx,
    run: Option<Run>,
    op_running: bool,
    next_run: u64,
    button: Option<Entity<AiButton>>,
    /// How often streamed text was applied to the message field (tests of the throttle).
    streamed_applies: usize,
    _subscriptions: Vec<Subscription>,
}

/// The controllers by tab, so the commit box's slot function finds the controller of its tab.
#[derive(Default)]
struct Controllers(HashMap<TabId, WeakEntity<AiController>>);

impl Global for Controllers {}

/// The controller of the tab `id`.
pub(super) fn of(id: TabId, cx: &App) -> Option<Entity<AiController>> {
    cx.try_global::<Controllers>()?.0.get(&id)?.upgrade()
}

impl AiController {
    /// The controller of the tab of `ctx`.
    pub fn new(ctx: TabCtx, cx: &mut Context<AiController>) -> AiController {
        let weak = cx.weak_entity();
        let controllers = cx.default_global::<Controllers>();
        // Controllers of closed tabs are gone: forget them.
        controllers
            .0
            .retain(|_, controller| controller.upgrade().is_some());
        controllers.0.insert(ctx.id, weak);
        let subscriptions =
            vec![cx.subscribe(
                &ctx.events,
                |this, _, event: &RepoTabEvent, cx| match event {
                    RepoTabEvent::OpStarted { kind } => {
                        this.op_running = true;
                        // Committing while the text still streams in commits what is there; the stream ends.
                        if matches!(
                            kind,
                            OpKind::Commit | OpKind::Amend | OpKind::ContinueOperation
                        ) {
                            this.stop(cx);
                        }
                        cx.notify();
                    }
                    RepoTabEvent::OpFinished { .. } => {
                        this.op_running = false;
                        cx.notify();
                    }
                    _ => {}
                },
            )];
        AiController {
            ctx,
            run: None,
            op_running: false,
            next_run: 0,
            button: None,
            streamed_applies: 0,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// Whether a generation runs.
    pub fn is_generating(&self) -> bool {
        self.run.is_some()
    }

    /// Whether an operation of the tab runs (tracked from the tab's events).
    pub fn op_running(&self) -> bool {
        self.op_running
    }

    /// How often streamed text was applied to the message field.
    #[cfg(test)]
    pub(crate) fn streamed_applies(&self) -> usize {
        self.streamed_applies
    }

    /// The button of the commit box, created on first use. `commit_box` is the box that renders it now.
    pub(super) fn button(
        &mut self,
        commit_box: WeakEntity<CommitBox>,
        cx: &mut Context<Self>,
    ) -> Entity<AiButton> {
        if let Some(button) = &self.button {
            button.update(cx, |b, _| b.set_commit_box(commit_box));
            return button.clone();
        }
        let controller = cx.entity();
        let ctx = self.ctx.clone();
        let button = cx.new(|cx| AiButton::new(ctx, &controller, commit_box, cx));
        self.button = Some(button.clone());
        button
    }

    /// Starts generating a commit message into `commit_box` (the Generate button). Does nothing while a
    /// generation runs; opens the AI settings when AI is not set up; asks for the one-time privacy confirmation
    /// before the first use of a provider that is not on this machine.
    pub fn generate(
        &mut self,
        commit_box: WeakEntity<CommitBox>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run.is_some() {
            return;
        }
        if !matches!(hub::readiness(cx), Readiness::Ready { .. }) {
            crate::dialogs::settings::open(
                window,
                cx,
                Some(crate::dialogs::settings::SettingsPage::Ai),
            );
            return;
        }
        if !jobs::needs_consent(cx) {
            self.start(commit_box, window, cx);
            return;
        }
        // The confirmation answers later, outside this update.
        let this = cx.weak_entity();
        jobs::with_consent(
            window,
            cx,
            move |window, cx| {
                this.update(cx, |controller, cx| {
                    controller.start(commit_box, window, cx);
                })
                .ok();
            },
            |_, _| {},
        );
    }

    /// Starts the real generation: the staged patch plus the message box text go to the service.
    fn start(
        &mut self,
        commit_box: WeakEntity<CommitBox>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run.is_some() {
            return;
        }
        let tab = Some(self.ctx.id);
        if let Some(refusal) = jobs::offline_refusal(cx) {
            crate::app_view::show_toast(
                cx,
                tab,
                crate::notifications::ToastKind::Error,
                refusal,
                None,
            );
            return;
        }
        let (Some(repo), Some(services)) = (self.ctx.backend.repo().cloned(), app::services(cx))
        else {
            crate::app_view::show_toast(
                cx,
                tab,
                crate::notifications::ToastKind::Error,
                "AI is unavailable here: the repository or the integrations are not open",
                None,
            );
            return;
        };
        let Some(box_entity) = commit_box.upgrade() else {
            return;
        };
        let notes = box_entity.read(cx).message(cx);
        let style = backend_ai::commit_style(settings::get_or_default(cx).ai_commit_style);
        self.start_with(
            commit_box,
            notes.clone(),
            window,
            cx,
            move |cancel, emit| {
                let mut on_text = |text: &str| emit(text.to_owned());
                backend_ai::generate_commit_message(
                    &services,
                    &repo,
                    notes,
                    style,
                    cancel,
                    &mut on_text,
                )
            },
        );
    }

    /// Starts a run of `job` whose streamed texts replace the message of `commit_box`.
    pub(crate) fn start_with(
        &mut self,
        commit_box: WeakEntity<CommitBox>,
        original: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        job: impl FnOnce(&CancelToken, &mut (dyn FnMut(String) + Send)) -> Result<String, AiFailure>
        + Send
        + 'static,
    ) {
        let Some(box_entity) = commit_box.upgrade() else {
            return;
        };
        self.next_run += 1;
        let id = self.next_run;
        box_entity.update(cx, |b, cx| b.set_read_only(true, cx));

        let this = cx.weak_entity();
        let home = window.window_handle();
        let escape = cx.intercept_keystrokes({
            let this = this.clone();
            move |event, window, cx| {
                if event.keystroke.key == "escape" && window.window_handle() == home {
                    let stopped = this
                        .update(cx, |controller, cx| controller.stop_run(id, cx))
                        .unwrap_or(false);
                    if stopped {
                        cx.stop_propagation();
                    }
                }
            }
        });
        let tab_switch = cx
            .try_global::<AppViewGlobal>()
            .and_then(|g| g.0.upgrade())
            .map(|view| {
                cx.observe(&view, |this, view, cx| {
                    let active = view
                        .read(cx)
                        .active_tab()
                        .is_some_and(|tab| tab.entity_id() == this.ctx.tab.entity_id());
                    if !active {
                        this.stop(cx);
                    }
                })
            });

        let on_item_this = this.clone();
        let on_done_this = this;
        let ai_job = jobs::drive(
            window,
            cx,
            job,
            move |text, window, cx| {
                on_item_this
                    .update(cx, |controller, cx| controller.apply(id, text, window, cx))
                    .ok();
            },
            move |result, window, cx| {
                on_done_this
                    .update(cx, |controller, cx| {
                        controller.finish(id, result, window, cx)
                    })
                    .ok();
            },
        );
        self.run = Some(Run {
            id,
            job: ai_job,
            commit_box,
            original,
            _escape: escape,
            _tab_switch: tab_switch,
        });
        cx.notify();
    }

    /// Applies streamed text of run `id` to the message field.
    pub(super) fn apply(
        &mut self,
        id: u64,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.run.as_ref().filter(|run| run.id == id) else {
            return;
        };
        if let Some(box_entity) = run.commit_box.upgrade() {
            self.streamed_applies += 1;
            box_entity.update(cx, |b, cx| b.set_message(text, window, cx));
        }
    }

    /// The run `id` ended with `result`.
    fn finish(
        &mut self,
        id: u64,
        result: Result<String, AiFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.run.take_if(|run| run.id == id) else {
            return;
        };
        let box_entity = run.commit_box.upgrade();
        if let Some(box_entity) = &box_entity {
            box_entity.update(cx, |b, cx| b.set_read_only(false, cx));
        }
        match result {
            Ok(message) => {
                if let Some(box_entity) = &box_entity {
                    box_entity.update(cx, |b, cx| b.set_message(message, window, cx));
                }
            }
            // The user stopped it: the text so far stays.
            Err(AiFailure::Cancelled) => {}
            Err(failure) => {
                if let Some(box_entity) = &box_entity {
                    let original = run.original;
                    box_entity.update(cx, |b, cx| b.set_message(original, window, cx));
                }
                jobs::show_failure(
                    cx,
                    Some(self.ctx.id),
                    "Couldn't generate the commit message",
                    &failure,
                );
            }
        }
        cx.notify();
    }

    /// Cancels the running generation and keeps the text received so far. Returns whether one ran.
    pub fn stop(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(run) = self.run.take() else {
            return false;
        };
        run.job.cancel();
        if let Some(box_entity) = run.commit_box.upgrade() {
            box_entity.update(cx, |b, cx| b.set_read_only(false, cx));
        }
        cx.notify();
        true
    }

    fn stop_run(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if self.run.as_ref().is_some_and(|run| run.id == id) {
            self.stop(cx)
        } else {
            false
        }
    }
}
