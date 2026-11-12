//! The New tag dialog (G10; 09 §3.4).

use gpui_kit::{App, Window};

use super::{SingleFieldConfig, SingleTextFieldDialog};
use crate::backend::ObjectId;
use crate::icons::AppIcon;
use crate::tab::TabCtx;

/// The configuration of the New tag dialog; `on_accept` receives the trimmed name.
pub fn new_tag_config(
    on_accept: impl Fn(String, &mut Window, &mut App) + 'static,
) -> SingleFieldConfig {
    let mut config = SingleFieldConfig::new(
        AppIcon::Tag,
        "New tag",
        "Create tag",
        move |name, window, cx| on_accept(name.trim().to_owned(), window, cx),
    );
    config.subtitle = Some("Create a new tag on the specified commit".into());
    config
}

/// Opens the New tag dialog for a tag on `target`. Accepting creates a lightweight tag.
pub fn open_new_tag(window: &mut Window, cx: &mut App, tab: TabCtx, target: ObjectId) {
    let config = new_tag_config(move |name, window, cx| {
        tab.with_tab(window, cx, move |t, window, cx| {
            t.create_tag(name, target, None, window, cx);
        });
    });
    SingleTextFieldDialog::open(window, cx, config);
}
