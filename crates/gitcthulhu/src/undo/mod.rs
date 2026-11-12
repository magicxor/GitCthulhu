//! Undo / Redo UI (D0.3; owned by X2 after G0b; spec 21 §5.1, §2.13.3). Milestone 1 ships the slots with their
//! exact M1 behaviour: two disabled toolbar buttons, no-op actions, no toast actions, an idle per-tab controller.
//! The oplog already records operations in M1 (`model.undo` is loaded).

use gpui_kit::{
    AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, Window, div,
};

use crate::icons::AppIcon;
use crate::metrics;
use crate::notifications::ToastAction;
use crate::tab::TabCtx;
use crate::tab::ops::{OpKind, RestoreKind};
use crate::widgets::{ExtendedMenuButton, MenuButton};

/// Element ids of the undo slot.
pub mod ids {
    /// The Undo split button.
    pub const TOOLBAR_UNDO: &str = "toolbar-undo";
    /// The Redo button.
    pub const TOOLBAR_REDO: &str = "toolbar-redo";
}

/// Tooltip of the disabled milestone-1 buttons.
pub const M1_TOOLTIP: &str = "Undo arrives in a later version";

/// The per-tab undo controller (X2: history popover, tooltips from `model.undo`). Idle in milestone 1.
pub struct UndoController {
    ctx: TabCtx,
}

impl UndoController {
    /// The controller of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _cx: &mut Context<UndoController>) -> UndoController {
        UndoController { ctx }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

/// The first child of the toolbar's centred group, before Pull: Undo (64×56 split button, margin end 4) and Redo
/// (56×56). Milestone 1: both disabled with the tooltip "Undo arrives in a later version".
pub fn toolbar_slot(_ctx: &TabCtx, _window: &mut Window, _cx: &mut App) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .child(
            ExtendedMenuButton::new(ids::TOOLBAR_UNDO, AppIcon::UndoOp, "Undo")
                .enabled(false)
                .tooltip(M1_TOOLTIP)
                .margins(0., metrics::TOOLBAR_ITEM_GAP),
        )
        .child(
            MenuButton::new(ids::TOOLBAR_REDO, AppIcon::RedoOp, "Redo")
                .enabled(false)
                .tooltip(M1_TOOLTIP),
        )
        .into_any_element()
}

/// The `Undo` action of the repository frame. Milestone 1: no-op.
pub fn on_undo(_ctx: &TabCtx, _window: &mut Window, _cx: &mut App) {}

/// The `Redo` action of the repository frame. Milestone 1: no-op.
pub fn on_redo(_ctx: &TabCtx, _window: &mut Window, _cx: &mut App) {}

/// What a success toast is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastContext {
    /// An operation that the oplog recorded.
    Recorded { kind: OpKind },
    /// A restore that completed.
    Restored { kind: RestoreKind },
}

/// The action of a success toast ("Undo" after a recorded operation, "Redo" after an undo). Milestone 1: none.
pub fn toast_action(_ctx: &TabCtx, _what: ToastContext) -> Option<ToastAction> {
    None
}

/// Registers the undo UI's bindings and hooks (X2).
pub fn register(_cx: &mut App) {}
