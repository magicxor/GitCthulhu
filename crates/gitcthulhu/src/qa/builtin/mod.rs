//! The built-in verbs: everything the merged foundation and the placeholder regions can support. Regions add their
//! own verbs with `qa::register_verb` (see `qa::verbs`).
//!
//! | Area | Verbs |
//! |---|---|
//! | App | `new tab`, `open repository`, `select tab`, `close tab`, `reload`, `resize`, `set theme`, `set scale`, `set diff`, `toggle diff`, `set option`, `key`, `type`, `action`, `close dialog`, `close menu` |
//! | Selection | `select uncommitted row`, `select log row`, `select commit`, `select branch`, `select tag`, `select stash`, `select file`, `clear file selection`, `open diff`, `open blame`, `open history`, `close view` |
//! | Menus | `open context menu <target>` |
//! | Dialogs | `open dialog <name>`, `open settings page <name>` |
//! | Checks | `assert no error toast`, `assert no error dialog`, `assert no error`, `assert toast`, `assert tab count`, `assert tab title`, `assert dialog`, `assert log rows` |
//! | Runner | `wait idle`, `wait <ms>`, `shot`, `quit` |

use gpui_kit::{App, Entity};

use super::verbs::{RunnerVerb, Verb, register_verb};
use crate::app_view::{AppView, AppViewGlobal};
use crate::tab::TabCtx;

mod app;
mod asserts;
mod commits;
mod dialogs;
mod menus;
mod select;

/// Registers every built-in verb.
pub fn register_all(cx: &mut App) {
    register_verb(
        cx,
        Verb::runner(
            "wait idle",
            "wait idle [ms]",
            "Waits until operations finished, loads settled and frames were drawn (fails after [ms], default 20000)",
            RunnerVerb::WaitIdle,
        ),
    );
    register_verb(
        cx,
        Verb::runner(
            "wait",
            "wait <ms>",
            "Pauses for a fixed time",
            RunnerVerb::Wait,
        ),
    );
    register_verb(
        cx,
        Verb::runner(
            "shot",
            "shot <name> [window]",
            "Writes <name>.png (the window's client area, or with `window` the whole window incl. the title bar) into the output directory",
            RunnerVerb::Shot,
        ),
    );
    register_verb(
        cx,
        Verb::runner(
            "quit",
            "quit [exit code]",
            "Ends the scenario and exits (also implied at the end of the file)",
            RunnerVerb::Quit,
        ),
    );
    app::register(cx);
    select::register(cx);
    menus::register(cx);
    dialogs::register(cx);
    asserts::register(cx);
}

/// The window's `AppView`.
pub(super) fn app_view(cx: &App) -> Result<Entity<AppView>, String> {
    cx.try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .ok_or_else(|| "the window content is gone".to_owned())
}

/// The context of the selected repository tab.
pub(super) fn active_ctx(cx: &App) -> Result<TabCtx, String> {
    let view = app_view(cx)?;
    let view = view.read(cx);
    let tab = view
        .active_tab()
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())?;
    Ok(tab.read(cx).ctx().clone())
}
