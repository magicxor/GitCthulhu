//! Author dialogs (owned by G10 after G0b): Author settings (09 §3.10) and Commit author identity (09 §3.11, asked
//! before a commit when Git has no identity).

use std::rc::Rc;

use gpui_kit::{App, Window};

use crate::backend::Identity;
use crate::tab::TabCtx;

/// What the Commit author identity dialog decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityChoice {
    /// Use this identity for one commit (`CommitOptions::identity_override`).
    Override(Identity),
    /// The identity was saved (`save_author`); commit normally.
    Saved,
}

/// Opens the Author settings dialog (global and, with a tab, repository identity).
pub fn open_author_settings(_window: &mut Window, cx: &mut App, tab: Option<TabCtx>) {
    crate::app_view::not_implemented(cx, tab.map(|t| t.id), "Author settings");
}

/// Opens the Commit author identity dialog; `on_done` receives the decision (not called on Cancel).
pub fn open_commit_identity(
    _window: &mut Window,
    cx: &mut App,
    tab: TabCtx,
    _on_done: Rc<dyn Fn(IdentityChoice, &mut Window, &mut App)>,
) {
    crate::app_view::not_implemented(cx, Some(tab.id), "Commit author identity");
}
