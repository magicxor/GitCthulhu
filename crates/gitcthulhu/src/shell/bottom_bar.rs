//! The bottom info bar (07 §7): the user identity at the left (a click opens the Author dialog), the update notice
//! slot and the version link at the right. The 1 dp divider above it is drawn by the page that hosts the bar.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::{dims, ids, platform};
use crate::backend::IdentityInfo;
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::observed;

/// Shown instead of a missing `user.name`.
pub const NAME_NOT_SET: &str = "Name not set";
/// Shown instead of a missing `user.email`.
pub const EMAIL_NOT_SET: &str = "Email not set";

/// The identity line of the bar: what `git commit` would use, else the configured parts with "Name not set" /
/// "Email not set" for the missing ones (local settings win over global ones).
pub fn identity_label(info: &IdentityInfo) -> String {
    let (name, email) = match &info.effective {
        Some(identity) => (identity.name.as_str(), identity.email.as_str()),
        None => (
            info.local
                .name
                .as_deref()
                .or(info.global.name.as_deref())
                .unwrap_or(NAME_NOT_SET),
            info.local
                .email
                .as_deref()
                .or(info.global.email.as_deref())
                .unwrap_or(EMAIL_NOT_SET),
        ),
    };
    format!("{name} <{email}>")
}

/// The text of the version link.
pub fn version_label() -> String {
    format!("Version {}", env!("CARGO_PKG_VERSION"))
}

/// The bottom bar of a repository tab (`ctx = Some`) or of the welcome page.
pub struct BottomBarView {
    ctx: Option<TabCtx>,
}

impl BottomBarView {
    /// A bottom bar for the tab of `ctx` (or the welcome page with `None`).
    pub fn new(ctx: Option<TabCtx>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx }
    }

    /// The tab of this bar, if any.
    pub fn ctx(&self) -> Option<&TabCtx> {
        self.ctx.as_ref()
    }
}

impl Render for BottomBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let identity = self.ctx.as_ref().and_then(|ctx| {
            let model = ctx.model.read(cx);
            let label = model.identity.value.as_deref().map(identity_label)?;
            Some((ctx.clone(), SharedString::from(label)))
        });
        let slot = crate::update::bottom_bar_slot(window, cx);
        bar(tokens, identity, slot)
    }
}

/// The bar itself: the clickable identity (when known), the optional update slot with its 16 dp end padding and the
/// version link.
pub(crate) fn bar(
    tokens: Tokens,
    identity: Option<(TabCtx, SharedString)>,
    slot: Option<AnyElement>,
) -> impl IntoElement {
    observed(crate::ids::BOTTOM_BAR)
        .flex_none()
        .w_full()
        .h(dp(metrics::BOTTOM_BAR_H))
        .px(dp(metrics::BOTTOM_BAR_PAD_H))
        .flex()
        .flex_row()
        .items_center()
        .bg(tokens.surface)
        .when_some(identity, |bar, (ctx, label)| {
            bar.child(
                div()
                    .id(ids::BOTTOM_IDENTITY)
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .on_click(move |_, window, cx| {
                        crate::dialogs::author::open_author_settings(window, cx, Some(ctx.clone()))
                    })
                    .text_body2()
                    .text_color(tokens.on_background)
                    .child(label)
                    .test_support(),
            )
        })
        .child(div().flex_1())
        .when_some(slot, |bar, slot| {
            bar.child(
                div()
                    .flex_none()
                    .pr(dp(dims::BOTTOM_UPDATE_PAD_END))
                    .child(slot),
            )
        })
        .child(
            div()
                .id(ids::BOTTOM_VERSION)
                .flex_none()
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(move |s| s.bg(tokens.hover_layer()))
                .active(move |s| s.bg(tokens.press_layer()))
                .on_click(|_, _, cx| platform::open_url(cx, crate::update::RELEASES_URL))
                .text_body2()
                .text_color(tokens.on_background)
                .whitespace_nowrap()
                .child(version_label())
                .test_support(),
        )
}
