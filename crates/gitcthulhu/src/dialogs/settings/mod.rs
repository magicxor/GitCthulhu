//! The Settings dialog (owned by G11 after G0b; 09 §3.18, spec 21 §5.4): 1000×720 frame, category list, setting
//! rows. The INTEGRATIONS entries, the Updates entry and the embedded-terminal rows appear only when their
//! `*_available(cx)` slots return true (milestone 1: never).

use gpui_kit::{App, Window};

pub mod accounts;
pub mod ai;
pub mod pages;
pub mod terminal;
pub mod updates;

/// A category of the Settings dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsPage {
    Appearance,
    Layout,
    DateTime,
    Environment,
    Branches,
    RemoteActions,
    Proxy,
    Authentication,
    Security,
    Accounts,
    Ai,
    Terminal,
    Updates,
    Logs,
}

/// Opens the Settings dialog, optionally at `page`.
pub fn open(_window: &mut Window, cx: &mut App, _page: Option<SettingsPage>) {
    crate::app_view::not_implemented(cx, None, "Settings");
}

/// Registers the Settings dialog's key bindings (G11).
pub fn register(_cx: &mut App) {}
