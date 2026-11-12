//! The diff text-selection menu (owned by G7b after G0b; 09 §4.7): Copy and Select all.

use std::rc::Rc;

use gpui_kit::{App, Window};

use super::MenuItem;
use crate::icons::AppIcon;

/// The selection menu; Copy is enabled iff text is selected.
pub fn selection_menu(
    has_selection: bool,
    copy: Rc<dyn Fn(&mut Window, &mut App)>,
    select_all: Rc<dyn Fn(&mut Window, &mut App)>,
) -> Vec<MenuItem> {
    vec![
        MenuItem::Action {
            id: "diff-copy".into(),
            icon: Some(AppIcon::Copy),
            label: "Copy".into(),
            enabled: has_selection,
            on_click: copy,
        },
        MenuItem::Action {
            id: "diff-select-all".into(),
            icon: None,
            label: "Select all".into(),
            enabled: true,
            on_click: select_all,
        },
    ]
}
