//! The commit box (owned by G5 after G0b; 08 §4.6, spec 21 §5.3). G0b fixes the public message API used by the AI
//! controller (X3); G5 implements the box (message field, Amend checkbox, Commit button, state buttons) and calls
//! `ai::commit_box_slot` at the right end of the checkbox row.

use gpui_kit::{Context, IntoElement, Render, Styled as _, Window};

use crate::tab::TabCtx;
use crate::widgets::observed;

/// Element id of the commit box.
pub const COMMIT_BOX: &str = "commit-box";

/// The commit box of a status pane.
pub struct CommitBox {
    ctx: TabCtx,
    message: String,
    read_only: bool,
}

impl CommitBox {
    /// The commit box of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            ctx,
            message: String::new(),
            read_only: false,
        }
    }

    /// The tab of this box.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The current message text.
    pub fn message(&self, _cx: &gpui_kit::App) -> String {
        self.message.clone()
    }

    /// Replaces the message text (streamed AI text, Amend prefill).
    pub fn set_message(&mut self, text: String, _window: &mut Window, cx: &mut Context<Self>) {
        if self.message != text {
            self.message = text;
            cx.notify();
        }
    }

    /// Makes the message field read-only (while AI text streams in).
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.read_only != read_only {
            self.read_only = read_only;
            cx.notify();
        }
    }

    /// Whether the message field is read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
}

impl Render for CommitBox {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(COMMIT_BOX).w_full()
    }
}
