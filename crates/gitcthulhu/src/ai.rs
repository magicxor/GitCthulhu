//! AI commit messages (D0.2; owned by X3 after G0b; spec 21 §5.3, §5.4, §2.13.3; 16 A7-A10).
//!
//! The commit box shows a "Generate" button ([`commit_box_slot`]) that streams a message for the staged changes
//! into the message field; Settings gets an AI page (`dialogs::settings::ai`) where the provider, model, endpoint
//! and API key are set. The service itself (`gitcthulhu-services`) and the keychain are reached only through
//! `backend::ai`. Pull-request text and branch-name suggestions are offered to other packages as [`generate_pr_text`]
//! and [`suggest_branch_name`]. Without the `services` feature there is no AI: no button, no settings page.

#[cfg(not(feature = "services"))]
use gpui_kit::Context;
use gpui_kit::{AnyElement, App, WeakEntity, Window};

use crate::status_pane::commit_box::CommitBox;
use crate::tab::TabCtx;

#[cfg(feature = "services")]
mod button;
#[cfg(feature = "services")]
mod controller;
#[cfg(any(test, feature = "qa"))]
#[cfg(feature = "services")]
pub(crate) mod fake;
#[cfg(feature = "services")]
pub(crate) mod hub;
#[cfg(feature = "services")]
mod jobs;
#[cfg(feature = "services")]
pub(crate) mod metrics;
#[cfg(feature = "qa")]
#[cfg(feature = "services")]
mod qa;
#[cfg(test)]
#[cfg(feature = "services")]
mod tests;

#[cfg(feature = "services")]
pub use button::{ButtonAction, ButtonState, button_state, ids};
#[cfg(feature = "services")]
pub use controller::AiController;
#[cfg(feature = "services")]
pub use hub::{AiHub, KeyStoreGlobal, Readiness, key_store, keys_changed, readiness};
#[cfg(feature = "services")]
pub(crate) use jobs::offline_refusal;
#[cfg(feature = "services")]
pub use jobs::{
    AiHandle, AiJob, BranchNameSource, generate_pr_text, is_ready, suggest_branch_name,
};
#[cfg(feature = "services")]
pub(crate) use metrics::KEY_DEBOUNCE_MS;

/// The per-tab AI controller. Without the `services` feature it is idle and has no button.
#[cfg(not(feature = "services"))]
pub struct AiController {
    ctx: TabCtx,
}

#[cfg(not(feature = "services"))]
impl AiController {
    /// The controller of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _cx: &mut Context<AiController>) -> AiController {
        AiController { ctx }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

/// The "Generate" button at the right end of the commit box's checkbox row (21 §5.3). `None` while the services
/// are not running (no button then: the row keeps Gitnuro's content) and in builds without the `services` feature.
pub fn commit_box_slot(
    ctx: &TabCtx,
    commit_box: WeakEntity<CommitBox>,
    _window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    #[cfg(feature = "services")]
    {
        use gpui_kit::IntoElement as _;
        crate::app::services(cx)?;
        hub::ensure(cx);
        let controller = controller::of(ctx.id, cx)?;
        let button = controller.update(cx, |c, cx| c.button(commit_box, cx));
        Some(button.into_any_element())
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (ctx, commit_box, cx);
        None
    }
}

/// Whether the settings dialog shows the AI page: the integrations run (feature `services` and the services started).
pub fn settings_available(cx: &App) -> bool {
    cfg!(feature = "services") && crate::app::services(cx).is_some()
}

/// Registers the AI hub, the QA verbs and the idle probe (X3).
pub fn register(cx: &mut App) {
    #[cfg(feature = "services")]
    {
        hub::install(cx);
        #[cfg(feature = "qa")]
        qa::register(cx);
    }
    #[cfg(not(feature = "services"))]
    let _ = cx;
}
