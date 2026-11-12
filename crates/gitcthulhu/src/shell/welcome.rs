//! The welcome page of an empty tab (07 §4): title, the home buttons, the recent repositories and the bottom bar.
//! The group is 928 dp wide, centred, and sits 25 % of the free height below the top.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestSupportExt as _, WeakEntity, Window, div,
    relative,
};

use super::bottom_bar::BottomBarView;
use super::platform;
use super::recent_repos::{OnOpen, RecentRepos};
use super::{REPOSITORY_URL, dims, ids};
use crate::app_view::AppView;
use crate::backend::GitStatus;
use crate::icons::AppIcon;
use crate::metrics::{self, dp};
use crate::settings::state;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{ButtonTile, IconTextButton, observed};

/// Key context of the welcome page.
pub const WELCOME_CONTEXT: &str = "Welcome";

/// The welcome page of an empty tab.
pub struct WelcomeView {
    app: WeakEntity<AppView>,
    focus: FocusHandle,
    recent: Entity<RecentRepos>,
    bottom_bar: Entity<BottomBarView>,
}

impl WelcomeView {
    /// The page of an empty tab of `app`.
    pub fn new(app: WeakEntity<AppView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let opener = app.clone();
        let on_open: OnOpen = Rc::new(move |path, window, cx| {
            opener
                .update(cx, |app, cx| app.open_repository(path, true, window, cx))
                .ok();
        });
        let recent = cx.new(|cx| RecentRepos::new(on_open, window, cx));
        let bottom_bar = cx.new(|cx| BottomBarView::new(None, window, cx));
        Self {
            app,
            focus: cx.focus_handle(),
            recent,
            bottom_bar,
        }
    }

    /// The window content this page belongs to (its tab API opens repositories).
    pub fn app(&self) -> &WeakEntity<AppView> {
        &self.app
    }

    /// The recent repositories list of the page.
    pub fn recent(&self) -> &Entity<RecentRepos> {
        &self.recent
    }

    /// Whether keyboard focus is anywhere inside the page (the page itself or the filter field).
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Focuses the page like Gitnuro does when it appears: the search field when there are recent repositories,
    /// else the page itself (07 §4.2). An open dialog (e.g. the error of a failed open) keeps the focus.
    pub fn focus_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        if state::get(cx).recent_repositories.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.recent
                .update(cx, |recent, cx| recent.focus_search(window, cx));
        }
    }
}

/// Asks for a directory and calls `then` on the window's `AppView` with it.
fn pick_then(
    app: WeakEntity<AppView>,
    window: &mut Window,
    cx: &mut App,
    then: fn(&mut AppView, PathBuf, &mut Window, &mut Context<AppView>),
) {
    platform::pick_directory(
        window,
        cx,
        Box::new(move |path, window, cx| {
            app.update(cx, |app, cx| then(app, path, window, cx)).ok();
        }),
    );
}

/// A flex child that takes `share` parts of the free space of its column.
fn spacer(share: f32) -> gpui_kit::Div {
    let mut spacer = div().flex_basis(relative(0.)).min_h_0();
    spacer.style().flex_grow = Some(share);
    spacer.style().flex_shrink = Some(1.);
    spacer
}

impl Render for WelcomeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let banner = match crate::app::git_status(cx) {
            GitStatus::Failed(err) => Some(format!("Git is not available: {err}")),
            _ => None,
        };
        let home_buttons = div()
            .flex_none()
            .w(dp(dims::WELCOME_BUTTON_W))
            .flex()
            .flex_col()
            .child(div().pb(dp(dims::WELCOME_TILE_PAD_BOTTOM)).child(
                ButtonTile::new(ids::WELCOME_OPEN, AppIcon::Open, "Open a repository").on_click({
                    let app = self.app.clone();
                    move |window, cx| {
                        pick_then(app.clone(), window, cx, |app, path, window, cx| {
                            app.open_repository(path, true, window, cx)
                        })
                    }
                }),
            ))
            .child(
                div().pb(dp(dims::WELCOME_TILE_PAD_BOTTOM)).child(
                    ButtonTile::new(ids::WELCOME_CLONE, AppIcon::Download, "Clone a repository")
                        .on_click(crate::dialogs::clone::open),
                ),
            )
            .child(
                div().pb(dp(dims::WELCOME_TILE_PAD_BOTTOM)).child(
                    ButtonTile::new(ids::WELCOME_INIT, AppIcon::Open, "Start a local repository")
                        .on_click({
                            let app = self.app.clone();
                            move |window, cx| {
                                pick_then(app.clone(), window, cx, |app, path, window, cx| {
                                    app.init_repository(path, window, cx)
                                })
                            }
                        }),
                ),
            )
            .child(
                observed(ids::WELCOME_MORE)
                    .pt(dp(dims::WELCOME_MORE_PAD_TOP))
                    .pb(dp(dims::WELCOME_MORE_PAD_BOTTOM))
                    .text_h3()
                    .child("Additional information"),
            )
            .child(
                IconTextButton::new(ids::WELCOME_SOURCE, AppIcon::Code, "Source code")
                    .on_click(|_, cx| platform::open_url(cx, REPOSITORY_URL)),
            )
            .child(
                IconTextButton::new(ids::WELCOME_BUG, AppIcon::Bug, "Report a bug")
                    .on_click(|_, cx| platform::open_url(cx, &format!("{REPOSITORY_URL}/issues"))),
            )
            .child(
                IconTextButton::new(ids::WELCOME_ABOUT, AppIcon::Info, "Additional information")
                    .on_click(crate::dialogs::about::open),
            )
            .child(
                IconTextButton::new(ids::WELCOME_SETTINGS, AppIcon::Settings, "Settings")
                    .on_click(|window, cx| crate::dialogs::settings::open(window, cx, None)),
            );
        let content = div()
            .flex_none()
            .w(dp(dims::WELCOME_GROUP_W))
            .flex()
            .flex_col()
            .gap(dp(dims::WELCOME_ROW_GAP))
            .text_color(tokens.on_background)
            .child(
                observed(ids::WELCOME_TITLE)
                    .pb(dp(dims::WELCOME_TITLE_PAD_BOTTOM))
                    .text_h1()
                    .child("GitCthulhu"),
            )
            .children(banner.map(|text| {
                observed(ids::WELCOME_GIT_BANNER)
                    .text_body1()
                    .text_color(tokens.error)
                    .child(text)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(dp(dims::WELCOME_COLUMN_GAP))
                    .child(home_buttons)
                    .child(
                        div()
                            .flex_none()
                            .pl(dp(dims::WELCOME_RECENT_PAD_START))
                            .child(
                                div()
                                    .w(dp(dims::RECENT_W))
                                    .h(dp(dims::RECENT_H))
                                    .child(self.recent.clone()),
                            ),
                    ),
            );
        div()
            .id(ids::WELCOME)
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.surface)
            .key_context(WELCOME_CONTEXT)
            .track_focus(&self.focus)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(spacer(dims::WELCOME_TOP_SHARE))
                    .child(
                        div()
                            .id(ids::WELCOME_GROUP)
                            .flex_none()
                            .child(content)
                            .test_support(),
                    )
                    .child(spacer(dims::WELCOME_BOTTOM_SHARE)),
            )
            .child(
                observed(ids::WELCOME_DIVIDER)
                    .flex_none()
                    .w_full()
                    .h(dp(metrics::FRAME_DIVIDER_H))
                    .bg(tokens.primary_variant.opacity(0.2)),
            )
            .child(self.bottom_bar.clone())
            .test_support()
    }
}
