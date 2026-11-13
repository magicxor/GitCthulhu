//! The registration hook (spec 21 §2.13.1): every region registers its key bindings, global action handlers, menu
//! extenders and hooks from its own `register(cx)`. This list is written once with every package; packages never
//! edit it.

use gpui_kit::App;

/// Called once by `actions::init` after the G0 bindings.
pub fn register_all(cx: &mut App) {
    crate::shell::register(cx);
    crate::toolbar::register(cx);
    crate::side_panel::register(cx);
    crate::log_view::register(cx);
    crate::status_pane::register(cx);
    crate::commit_changes::register(cx);
    crate::diff::register(cx);
    crate::diff::selection::register(cx);
    crate::blame::register(cx);
    crate::file_history::register(cx);
    crate::rebase_interactive::register(cx);
    crate::dialogs::register::register(cx);
    crate::dialogs::settings::register(cx);
    crate::terminal::register(cx);
    crate::undo::register(cx);
    crate::ai::register(cx);
    crate::forge::register(cx);
    crate::update::register(cx);
    crate::worktrees::register(cx);
}
