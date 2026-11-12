//! The embedded terminal (D0.4; owned by X1 after G0b; spec 21 §5.2, §4.7, §2.13.3). The real implementation lives
//! behind `#[cfg(feature = "terminal")]` (off in milestone 1). Milestone 1: a disabled toolbar button, a zero-height
//! panel slot, no-op toggle, no close confirmation.

use gpui_kit::{AnyElement, App, Entity, Hsla, SharedString, Styled as _, Window, div};

use crate::backend::prompt::ConfirmRequest;
use crate::ids;
use crate::tab::TabCtx;
use crate::widgets::ObservedExt as _;

/// Tooltip of the disabled milestone-1 button.
pub const M1_TOOLTIP: &str = "The embedded terminal arrives in a later version";

/// How the toolbar's Terminal button looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalButtonState {
    pub enabled: bool,
    pub tooltip: SharedString,
    /// The status dot at the icon's top-right (Working / Blocked / Failed).
    pub status_dot: Option<Hsla>,
}

/// The terminal panel of a tab (X1).
pub struct TerminalPanel {
    ctx: TabCtx,
}

impl TerminalPanel {
    /// The panel of the tab of `ctx`.
    pub fn new(ctx: TabCtx) -> Self {
        Self { ctx }
    }

    /// The tab of this panel.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

/// The state of the toolbar's Terminal button. Milestone 1: disabled, no dot.
pub fn toolbar_button_state(_ctx: &TabCtx, _cx: &App) -> TerminalButtonState {
    TerminalButtonState {
        enabled: false,
        tooltip: M1_TOOLTIP.into(),
        status_dot: None,
    }
}

/// The terminal panel between the split and the frame divider. Milestone 1: a zero-height element `terminal-panel`.
pub fn panel_slot(_ctx: &TabCtx, _window: &mut Window, _cx: &mut App) -> AnyElement {
    div()
        .flex_none()
        .w_full()
        .h_0()
        .observed(ids::TERMINAL_PANEL)
}

/// Shows or hides the panel (`ToggleTerminal`). Milestone 1: no-op.
pub fn toggle(_ctx: &TabCtx, _window: &mut Window, _cx: &mut App) {}

/// The question asked before closing a tab whose terminal runs a working shell. Milestone 1: `None`.
pub fn confirm_close(_ctx: &TabCtx, _cx: &App) -> Option<ConfirmRequest> {
    None
}

/// Creates the tab's terminal panel. Milestone 1: `None`.
pub fn create_panel(
    _ctx: &TabCtx,
    _window: &mut Window,
    _cx: &mut App,
) -> Option<Entity<TerminalPanel>> {
    None
}

/// Whether the settings dialog shows the embedded-terminal rows. Milestone 1: `false`.
pub fn settings_available(_cx: &App) -> bool {
    false
}

/// Registers the terminal's bindings (`NoAction` for every ancestor key in the `Terminal` context, X1).
pub fn register(_cx: &mut App) {}
