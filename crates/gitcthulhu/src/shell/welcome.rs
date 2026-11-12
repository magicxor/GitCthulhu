//! The welcome page (owned by G1 after G0b; 07 §4). The placeholder fills the tab area, shows the Git banner when
//! Git is unavailable and offers an Open tile.

use gpui_kit::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, WeakEntity, Window, div,
};

use super::ids;
use crate::actions::OpenRepository;
use crate::app_view::AppView;
use crate::backend::GitStatus;
use crate::icons::AppIcon;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{ButtonTile, observed};

/// The welcome page of an empty tab.
pub struct WelcomeView {
    app: WeakEntity<AppView>,
}

impl WelcomeView {
    /// The page of an empty tab of `app`.
    pub fn new(app: WeakEntity<AppView>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { app }
    }

    /// The window content this page belongs to (its tab API opens repositories).
    pub fn app(&self) -> &WeakEntity<AppView> {
        &self.app
    }
}

impl Render for WelcomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let banner = match crate::app::git_status(cx) {
            GitStatus::Failed(err) => Some(err.to_string()),
            _ => None,
        };
        observed(ids::WELCOME)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(tokens.surface)
            .children(banner.map(|text| div().text_body1().text_color(tokens.error).child(text)))
            .child(
                ButtonTile::new(ids::WELCOME_OPEN, AppIcon::Open, "Open a repository")
                    .on_click(|window, cx| window.dispatch_action(Box::new(OpenRepository), cx)),
            )
    }
}
