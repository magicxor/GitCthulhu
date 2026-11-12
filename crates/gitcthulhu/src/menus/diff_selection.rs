//! The diff text-selection menu (G7b; 09 §4.7): Copy and Select All. The diff is read-only, so Cut and Paste never
//! apply; the menu opens only while text is selected (`diff::selection`).

use std::rc::Rc;

use gpui_kit::{App, Window};

use super::MenuItem;
use crate::icons::AppIcon;

/// Item ids (`menu-item-<id>` in the rendered menu).
pub mod ids {
    /// "Copy".
    pub const COPY: &str = "diff-copy";
    /// "Select All".
    pub const SELECT_ALL: &str = "diff-select-all";
}

/// The selection menu; Copy is enabled iff text is selected.
pub fn selection_menu(
    has_selection: bool,
    copy: Rc<dyn Fn(&mut Window, &mut App)>,
    select_all: Rc<dyn Fn(&mut Window, &mut App)>,
) -> Vec<MenuItem> {
    vec![
        MenuItem::Action {
            id: ids::COPY.into(),
            icon: Some(AppIcon::Copy),
            label: "Copy".into(),
            enabled: has_selection,
            on_click: copy,
        },
        MenuItem::Action {
            id: ids::SELECT_ALL.into(),
            icon: None,
            label: "Select All".into(),
            enabled: true,
            on_click: select_all,
        },
    ]
}
