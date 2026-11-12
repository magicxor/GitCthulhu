//! AI commit messages (D0.2; owned by X3 after G0b; spec 21 §5.3, §2.13.3). Milestone 1: no button in the commit
//! box, no settings page, an idle per-tab controller.

use gpui_kit::{AnyElement, App, Context, WeakEntity, Window};

use crate::status_pane::commit_box::CommitBox;
use crate::tab::TabCtx;

/// The per-tab AI controller (X3: generation, streaming into the commit box). Idle in milestone 1.
pub struct AiController {
    ctx: TabCtx,
}

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

/// The "Generate" button at the right end of the commit box's checkbox row. Milestone 1: `None`.
pub fn commit_box_slot(
    _ctx: &TabCtx,
    _commit_box: WeakEntity<CommitBox>,
    _window: &mut Window,
    _cx: &mut App,
) -> Option<AnyElement> {
    None
}

/// Whether the settings dialog shows the AI page. Milestone 1: `false`.
pub fn settings_available(_cx: &App) -> bool {
    false
}

/// Registers the AI UI's bindings and hooks (X3).
pub fn register(_cx: &mut App) {}
