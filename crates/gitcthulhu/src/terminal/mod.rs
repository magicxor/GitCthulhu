//! The embedded terminal (D0.4; spec 21 §5.2, §4.7, §2.13.3; terminal design 18 §5): the toolbar's Terminal button
//! toggles a bottom panel in the repository frame with one or more terminals on the repository's worktree.
//!
//! The terminal lives behind the `terminal` Cargo feature (on by default; `but-tty` builds libghostty-vt with
//! Zig 0.17). Without it (`--no-default-features --features services`) the button is disabled, the panel slot is
//! empty and the settings page keeps Gitnuro's disabled row.
//!
//! Each repository tab owns one [`TerminalPanel`] (created with the tab, dropped with it: dropping a terminal hangs
//! up its shell and ends the shell's process tree). The frame and the toolbar reach the panel through the slot
//! functions below, which look it up by tab id, so they never read the tab entity while it renders.

use gpui_kit::{AnyElement, App, Entity, Hsla, SharedString, Styled as _, Window, div};

#[cfg(feature = "terminal")]
use gpui_kit::{IntoElement as _, ParentElement as _};

use crate::backend::prompt::ConfirmRequest;
use crate::tab::TabCtx;
use crate::widgets::ObservedExt as _;

#[cfg(feature = "terminal")]
mod colors;
#[cfg(feature = "terminal")]
mod grid;
#[cfg(feature = "terminal")]
mod keys;
#[cfg(feature = "terminal")]
mod panel;
#[cfg(all(feature = "terminal", feature = "qa"))]
mod qa;
#[cfg(feature = "terminal")]
pub mod shell;
#[cfg(all(test, feature = "terminal"))]
mod tests;
#[cfg(feature = "terminal")]
mod view;

#[cfg(feature = "terminal")]
pub use panel::{PanelEvent, TerminalPanel};
#[cfg(feature = "terminal")]
pub use view::{TerminalEvent, TerminalView};

/// Element ids of the panel (tests, QA).
pub mod ids {
    /// The panel slot between the split and the frame divider (zero height while hidden).
    pub const PANEL: &str = crate::ids::TERMINAL_PANEL;
    /// The 8 dp resize gutter on the panel's top edge.
    pub const RESIZER: &str = "terminal-resizer";
    /// The box with the header and the terminal (x 8..W-8).
    pub const BOX: &str = "terminal-box";
    /// The 34 dp header with the terminal tabs.
    pub const HEADER: &str = "terminal-header";
    /// The area the active terminal fills.
    pub const BODY: &str = "terminal-body";
    /// A terminal tab: `(TAB, index)`.
    pub const TAB: &str = "terminal-tab";
    /// The status dot of a terminal tab: `(TAB_DOT, index)`.
    pub const TAB_DOT: &str = "terminal-tab-dot";
    /// The close button of a terminal tab: `(TAB_CLOSE, index)`.
    pub const TAB_CLOSE: &str = "terminal-tab-close";
    /// The "+" button (new terminal; right click: shell picker).
    pub const NEW: &str = "terminal-new";
    /// The hide button at the header's right end.
    pub const HIDE: &str = "terminal-hide";
    /// The terminal element (key context `Terminal`).
    pub const VIEW: &str = "terminal-view";
}

/// The key context of the terminal element: app shortcuts are neutralised there by `NoAction` bindings, so their
/// keys reach the shell.
pub const TERMINAL_CONTEXT: &str = "Terminal";

/// Tooltip of the button while the panel is hidden.
pub const SHOW_TOOLTIP: &str = "Show terminal";
/// Tooltip of the button while the panel shows.
pub const HIDE_TOOLTIP: &str = "Hide terminal";
/// Tooltip of the disabled button of a repository without a working tree (bare) or without a panel.
pub const NO_WORKTREE_TOOLTIP: &str =
    "The embedded terminal needs a repository with a working tree";
/// Tooltip of the disabled button of a build without the `terminal` feature.
pub const NOT_BUILT_TOOLTIP: &str = "This build of GitCthulhu has no embedded terminal";

/// How the toolbar's Terminal button looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalButtonState {
    pub enabled: bool,
    pub tooltip: SharedString,
    /// The status dot at the icon's top-right (Working / Blocked / Failed).
    pub status_dot: Option<Hsla>,
}

/// The terminal panel of a tab in a build without the `terminal` feature: never created.
#[cfg(not(feature = "terminal"))]
pub struct TerminalPanel {
    ctx: TabCtx,
}

#[cfg(not(feature = "terminal"))]
impl TerminalPanel {
    /// The tab of this panel.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

/// The panels by tab, weakly: a panel lives exactly as long as its tab.
#[cfg(feature = "terminal")]
#[derive(Default)]
struct Panels(std::collections::HashMap<crate::tab::TabId, gpui_kit::WeakEntity<TerminalPanel>>);

#[cfg(feature = "terminal")]
impl gpui_kit::Global for Panels {}

/// The panel of the tab of `ctx`, if it has one.
#[cfg(feature = "terminal")]
pub fn panel_of(ctx: &TabCtx, cx: &App) -> Option<Entity<TerminalPanel>> {
    cx.try_global::<Panels>()?.0.get(&ctx.id)?.upgrade()
}

/// The state of the toolbar's Terminal button: "Show terminal" / "Hide terminal" with the most urgent status of the
/// tab's terminals as the dot (21 §5.2).
pub fn toolbar_button_state(ctx: &TabCtx, cx: &App) -> TerminalButtonState {
    #[cfg(feature = "terminal")]
    {
        match panel_of(ctx, cx) {
            Some(panel) => {
                let panel = panel.read(cx);
                TerminalButtonState {
                    enabled: true,
                    tooltip: if panel.is_visible() {
                        HIDE_TOOLTIP
                    } else {
                        SHOW_TOOLTIP
                    }
                    .into(),
                    status_dot: panel
                        .aggregate_status(cx)
                        .and_then(|status| panel::status_color(&status, cx)),
                }
            }
            None => TerminalButtonState {
                enabled: false,
                tooltip: NO_WORKTREE_TOOLTIP.into(),
                status_dot: None,
            },
        }
    }
    #[cfg(not(feature = "terminal"))]
    {
        let _ = (ctx, cx);
        TerminalButtonState {
            enabled: false,
            tooltip: NOT_BUILT_TOOLTIP.into(),
            status_dot: None,
        }
    }
}

/// The terminal panel between the split and the frame divider: the tab's panel (zero height while hidden), or a
/// zero-height element `terminal-panel` when the tab has none.
pub fn panel_slot(ctx: &TabCtx, window: &mut Window, cx: &mut App) -> AnyElement {
    let _ = window;
    #[cfg(feature = "terminal")]
    if let Some(panel) = panel_of(ctx, cx) {
        return div().flex_none().w_full().child(panel).into_any_element();
    }
    let _ = (ctx, cx);
    div().flex_none().w_full().h_0().observed(ids::PANEL)
}

/// Shows or hides the panel (`ToggleTerminal`, the toolbar button). The first show opens a terminal with the
/// default shell; showing focuses the active terminal, hiding refocuses the repository frame.
pub fn toggle(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    #[cfg(feature = "terminal")]
    if let Some(panel) = panel_of(ctx, cx) {
        panel.update(cx, |panel, cx| panel.toggle(window, cx));
    }
    #[cfg(not(feature = "terminal"))]
    let _ = (ctx, window, cx);
}

/// The question asked before closing a tab whose terminal runs a program that reports it is working.
pub fn confirm_close(ctx: &TabCtx, cx: &App) -> Option<ConfirmRequest> {
    #[cfg(feature = "terminal")]
    {
        panel_of(ctx, cx)?.read(cx).confirm_close(cx)
    }
    #[cfg(not(feature = "terminal"))]
    {
        let _ = (ctx, cx);
        None
    }
}

/// Creates the tab's terminal panel: `None` for a repository without a working tree (bare) and for the inert test
/// backend, whose button stays disabled.
pub fn create_panel(
    ctx: &TabCtx,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<TerminalPanel>> {
    #[cfg(feature = "terminal")]
    {
        let workdir = ctx.backend.handle()?.workdir()?;
        let panel = TerminalPanel::create(ctx.clone(), workdir, window, cx);
        let weak = panel.downgrade();
        let panels = cx.default_global::<Panels>();
        panels.0.retain(|_, panel| panel.upgrade().is_some());
        panels.0.insert(ctx.id, weak);
        Some(panel)
    }
    #[cfg(not(feature = "terminal"))]
    {
        let _ = (ctx, window, cx);
        None
    }
}

/// Whether the settings dialog shows the embedded-terminal rows (`true` in builds with the terminal).
pub fn settings_available(_cx: &App) -> bool {
    cfg!(feature = "terminal")
}

/// Registers the terminal's bindings: `NoAction` in the `Terminal` context for every ancestor key (except Ctrl+`,
/// which keeps toggling the panel) and for Tab / Shift+Tab, so those keys reach the shell; plus the QA verbs.
pub fn register(cx: &mut App) {
    #[cfg(feature = "terminal")]
    {
        cx.bind_keys(keys::bindings());
        #[cfg(feature = "qa")]
        qa::register(cx);
    }
    #[cfg(not(feature = "terminal"))]
    let _ = cx;
}
