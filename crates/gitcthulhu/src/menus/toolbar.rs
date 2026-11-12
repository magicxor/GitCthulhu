//! The toolbar's Pull, Push and Stash dropdowns (owned by G2 after G0b; 09 §4.1). Milestone 1 placeholders: empty.

use gpui_kit::App;

use super::MenuItem;
use crate::tab::TabCtx;

/// The Pull dropdown.
pub fn pull_menu(_tab: &TabCtx, _cx: &App) -> Vec<MenuItem> {
    Vec::new()
}

/// The Push dropdown.
pub fn push_menu(_tab: &TabCtx, _cx: &App) -> Vec<MenuItem> {
    Vec::new()
}

/// The Stash dropdown.
pub fn stash_menu(_tab: &TabCtx, _cx: &App) -> Vec<MenuItem> {
    Vec::new()
}
