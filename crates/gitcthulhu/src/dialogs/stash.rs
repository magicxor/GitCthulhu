//! The Stash with message dialog (G10; 09 §3.5).

use gpui_kit::{App, Window};

use super::{SingleFieldConfig, SingleTextFieldDialog};
use crate::icons::AppIcon;
use crate::tab::TabCtx;

/// The configuration of the Stash with message dialog; `on_accept` receives the trimmed message.
pub fn stash_config(
    on_accept: impl Fn(String, &mut Window, &mut App) + 'static,
) -> SingleFieldConfig {
    let mut config = SingleFieldConfig::new(
        AppIcon::Stash,
        "Stash message",
        "Stash",
        move |message, window, cx| on_accept(message.trim().to_owned(), window, cx),
    );
    config.subtitle = Some("Create a new stash with a custom message".into());
    config
}

/// Opens the Stash with message dialog. Accepting stashes the changes, untracked files included (like the
/// toolbar's Stash button).
pub fn open_stash_with_message(window: &mut Window, cx: &mut App, tab: TabCtx) {
    let config = stash_config(move |message, window, cx| {
        tab.with_tab(window, cx, move |t, window, cx| {
            t.stash(Some(message), true, window, cx);
        });
    });
    SingleTextFieldDialog::open(window, cx, config);
}
