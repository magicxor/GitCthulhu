//! The header bar shared by the blame view and the file history view (09 §2.1, §2.2): 40 dp, `tertiary_surface`,
//! the file path, a close button (Gitnuro's `contentDescription` "Close blame"/"Close history" is an accessibility
//! label, not a tooltip, so the button has none).

use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, TestSupportExt as _, Window, div,
};

use crate::icons::AppIcon;
use crate::metrics::blame as m;
use crate::metrics::dp;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{IconButton, observed};

/// The header of a full-size blame or history view. `id` is the bar's element id, `close_id` the close button's,
/// `title_id` the path text's.
pub(crate) fn view_header(
    id: impl Into<ElementId>,
    close_id: impl Into<ElementId>,
    title_id: impl Into<ElementId>,
    path: SharedString,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let tokens = *cx.tokens();
    observed(id)
        .w_full()
        .h(dp(m::HEADER_H))
        .flex_none()
        .flex()
        .items_center()
        .px(dp(m::HEADER_PAD))
        .bg(tokens.tertiary_surface)
        .child(
            div()
                .id(title_id)
                .flex_shrink(1.)
                .min_w_0()
                .px(dp(m::HEADER_TITLE_PAD_H))
                .truncate()
                .text_body1()
                .text_color(tokens.on_background)
                .child(path)
                .test_support(),
        )
        .child(div().flex_1())
        .child(
            IconButton::new(close_id, AppIcon::Close)
                .icon_size(m::CLOSE_ICON)
                .on_click(on_close),
        )
}
