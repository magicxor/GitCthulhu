//! The recent repositories list (owned by G1 after G0b; 07 §4.4), reused by the toolbar's Open popup (G2).

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use super::ids;
use crate::widgets::observed;

/// Called with the repository the user picked.
pub type OnOpen = Rc<dyn Fn(PathBuf, &mut Window, &mut App)>;

/// The filterable list of `UiState::recent_repositories`.
pub struct RecentRepos {
    on_open: OnOpen,
}

impl RecentRepos {
    /// A list that calls `on_open` with the picked repository.
    pub fn new(on_open: OnOpen, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { on_open }
    }

    /// The callback of a picked repository.
    pub fn on_open(&self) -> &OnOpen {
        &self.on_open
    }
}

impl Render for RecentRepos {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::RECENT_REPOS).size_full()
    }
}
