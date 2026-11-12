//! The toolbar's Pull, Push and Stash dropdowns (owned by G2; 09 §4.1). The toolbar's split buttons show them under
//! their chevron strip with the icon column reserved.

use gpui_kit::App;

use super::MenuItem;
use crate::backend::PullMode;
use crate::icons::AppIcon;
use crate::tab::TabCtx;

/// The Pull dropdown: Pull with the opposite of the default type ("Pull with rebase", or "Pull with merge" when
/// "Pull with rebase as default" is on), then "Fetch all".
pub fn pull_menu(tab: &TabCtx, cx: &App) -> Vec<MenuItem> {
    let rebase_is_default = crate::settings::get_or_default(cx).pull_with_rebase;
    let (label, mode) = if rebase_is_default {
        ("Pull with merge", PullMode::Merge)
    } else {
        ("Pull with rebase", PullMode::Rebase)
    };
    let pull_tab = tab.clone();
    let fetch_tab = tab.clone();
    vec![
        MenuItem::action("pull-opposite", label, move |window, cx| {
            pull_tab.with_tab(window, cx, |tab, window, cx| {
                tab.pull(Some(mode), window, cx);
            })
        }),
        MenuItem::action("fetch-all", "Fetch all", move |window, cx| {
            fetch_tab.with_tab(window, cx, |tab, window, cx| {
                tab.fetch_all(window, cx);
            })
        }),
    ]
}

/// The Push dropdown: "Push including tags" and "Force push" (`--force-with-lease` when "Push with lease" is on).
pub fn push_menu(tab: &TabCtx, _cx: &App) -> Vec<MenuItem> {
    let tags_tab = tab.clone();
    let force_tab = tab.clone();
    vec![
        MenuItem::action("push-tags", "Push including tags", move |window, cx| {
            tags_tab.with_tab(window, cx, |tab, window, cx| {
                tab.push_current(false, true, window, cx);
            })
        })
        .with_icon(AppIcon::Tag),
        MenuItem::action("push-force", "Force push", move |window, cx| {
            force_tab.with_tab(window, cx, |tab, window, cx| {
                tab.push_current(true, false, window, cx);
            })
        }),
    ]
}

/// The Stash dropdown: "Stash with message" opens the dialog of 09 §3.5.
pub fn stash_menu(tab: &TabCtx, _cx: &App) -> Vec<MenuItem> {
    let tab = tab.clone();
    vec![
        MenuItem::action(
            "stash-with-message",
            "Stash with message",
            move |window, cx| {
                crate::dialogs::stash::open_stash_with_message(window, cx, tab.clone())
            },
        )
        .with_icon(AppIcon::Message),
    ]
}
