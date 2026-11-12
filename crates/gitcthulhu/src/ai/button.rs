//! The "Generate" button at the right end of the commit box's checkbox row (spec 21 §5.3).

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, WeakEntity, Window, div, percentage,
};

use super::controller::AiController;
use super::hub::{self, Readiness};
use super::metrics as m;
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::status_pane::commit_box::CommitBox;
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{TooltipExt as _, with_layer};

/// Element ids of the AI additions (tests, QA).
pub mod ids {
    /// The Generate / Stop button in the commit box.
    pub const BUTTON: &str = "ai-generate";
}

/// What a click on the button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    /// Start generating.
    Generate,
    /// Stop the running generation.
    Stop,
    /// Open Settings at the AI page.
    OpenSettings,
    /// Nothing (disabled).
    None,
}

/// How the button looks and behaves in one state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonState {
    /// "Generate" or "Stop".
    pub label: &'static str,
    /// `Sparkles`, or `Spinner` while streaming.
    pub icon: AppIcon,
    /// Whether a click does anything.
    pub enabled: bool,
    /// The tooltip.
    pub tooltip: String,
    /// What a click does.
    pub action: ButtonAction,
}

/// The state of the button (21 §5.3 "States"): streaming shows Stop; an AI that is not set up keeps the button
/// enabled and leads to the settings; nothing staged or a running operation disables it.
pub fn button_state(
    readiness: &Readiness,
    staged: usize,
    op_running: bool,
    generating: bool,
) -> ButtonState {
    let state = |label, icon, enabled, tooltip: &str, action| ButtonState {
        label,
        icon,
        enabled,
        tooltip: tooltip.to_owned(),
        action,
    };
    if generating {
        return state(
            "Stop",
            AppIcon::Spinner,
            true,
            "Stop generating (Esc)",
            ButtonAction::Stop,
        );
    }
    match readiness {
        Readiness::Pending => state(
            "Generate",
            AppIcon::Sparkles,
            false,
            "Checking the AI settings...",
            ButtonAction::None,
        ),
        Readiness::Off | Readiness::NotConfigured(_) => state(
            "Generate",
            AppIcon::Sparkles,
            true,
            "Set up an AI provider in Settings \u{2192} AI",
            ButtonAction::OpenSettings,
        ),
        Readiness::Ready { .. } if staged == 0 => state(
            "Generate",
            AppIcon::Sparkles,
            false,
            "Stage changes to generate a commit message",
            ButtonAction::None,
        ),
        Readiness::Ready { .. } if op_running => state(
            "Generate",
            AppIcon::Sparkles,
            false,
            "Wait for the running operation to finish",
            ButtonAction::None,
        ),
        Readiness::Ready { provider, model } => state(
            "Generate",
            AppIcon::Sparkles,
            true,
            &format!("Write a commit message for the staged changes with {provider} ({model})"),
            ButtonAction::Generate,
        ),
    }
}

/// The button entity of one tab's commit box (created and cached by the tab's [`AiController`]).
pub struct AiButton {
    ctx: TabCtx,
    controller: WeakEntity<AiController>,
    commit_box: WeakEntity<CommitBox>,
    _subscriptions: Vec<Subscription>,
}

impl AiButton {
    pub(super) fn new(
        ctx: TabCtx,
        controller: &Entity<AiController>,
        commit_box: WeakEntity<CommitBox>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = vec![
            cx.observe(controller, |_, _, cx| cx.notify()),
            cx.observe(&ctx.model, |_, _, cx| cx.notify()),
        ];
        if let Some(hub) = hub::hub(cx) {
            subscriptions.push(cx.observe(&hub, |_, _, cx| cx.notify()));
        }
        Self {
            ctx,
            controller: controller.downgrade(),
            commit_box,
            _subscriptions: subscriptions,
        }
    }

    /// Points the button at the commit box that renders it now.
    pub(super) fn set_commit_box(&mut self, commit_box: WeakEntity<CommitBox>) {
        self.commit_box = commit_box;
    }

    /// The state the button would render now.
    pub fn state(&self, cx: &gpui_kit::App) -> ButtonState {
        let staged = self
            .ctx
            .model
            .read(cx)
            .status
            .value
            .as_ref()
            .map_or(0, |status| status.staged.len());
        let (op_running, generating) = self
            .controller
            .upgrade()
            .map(|c| {
                let c = c.read(cx);
                (c.op_running(), c.is_generating())
            })
            .unwrap_or_default();
        button_state(&hub::readiness(cx), staged, op_running, generating)
    }

    fn click(&mut self, action: ButtonAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ButtonAction::None => {}
            ButtonAction::OpenSettings => crate::dialogs::settings::open(
                window,
                cx,
                Some(crate::dialogs::settings::SettingsPage::Ai),
            ),
            ButtonAction::Stop => {
                if let Some(controller) = self.controller.upgrade() {
                    controller.update(cx, |c, cx| {
                        c.stop(cx);
                    });
                }
            }
            ButtonAction::Generate => {
                if let Some(controller) = self.controller.upgrade() {
                    let commit_box = self.commit_box.clone();
                    controller.update(cx, |c, cx| c.generate(commit_box, window, cx));
                }
            }
        }
    }
}

impl Render for AiButton {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let state = self.state(cx);
        let color = if state.enabled {
            tokens.primary_variant
        } else {
            tokens.disabled_content()
        };
        let action = state.action;
        let enabled = state.enabled;
        div()
            .id(ids::BUTTON)
            .flex_none()
            .h(dp(m::BUTTON_H))
            .px(dp(m::BUTTON_PAD_H))
            .flex()
            .flex_row()
            .items_center()
            .rounded(dp(metrics::RADIUS))
            .text_body2()
            .text_color(color)
            .when(enabled, |b| {
                b.cursor_pointer()
                    .hover(move |s| s.bg(with_layer(tokens.background, tokens.hover_layer())))
                    .active(move |s| s.bg(with_layer(tokens.background, tokens.press_layer())))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.click(action, window, cx)),
                    )
            })
            .child(if state.icon == AppIcon::Spinner {
                icon(state.icon, m::BUTTON_ICON, color)
                    .with_animation(
                        "ai-generate-spinner",
                        Animation::new(Duration::from_millis(1000)).repeat(),
                        |icon, delta| icon.rotate(percentage(delta)),
                    )
                    .into_any_element()
            } else {
                icon(state.icon, m::BUTTON_ICON, color).into_any_element()
            })
            .child(
                div()
                    .ml(dp(m::BUTTON_ICON_GAP))
                    .whitespace_nowrap()
                    .child(state.label),
            )
            .delayed_tooltip(state.tooltip)
            .test_support()
    }
}
