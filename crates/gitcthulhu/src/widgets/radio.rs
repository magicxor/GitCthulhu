//! `RadioRow`: the radio cards of dialogs such as "Reset current branch" (Gitnuro 09 §3.6): a bordered card with a
//! 16 dp indicator circle, a body 1 title and a body 2 subtitle.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

/// One selectable card.
#[derive(IntoElement)]
pub struct RadioRow {
    id: ElementId,
    title: SharedString,
    subtitle: Option<SharedString>,
    selected: bool,
    width: Option<f32>,
    on_select: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl RadioRow {
    /// A card. `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: None,
            selected,
            width: None,
            on_select: None,
        }
    }

    /// The second line (13 sp, `on_background_secondary`).
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Fixed card width in dp (default: the parent's width).
    pub fn width(mut self, width_dp: f32) -> Self {
        self.width = Some(width_dp);
        self
    }

    /// Sets the handler run when the card is clicked.
    pub fn on_select(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for RadioRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let accent = if self.selected {
            tokens.primary
        } else {
            tokens.surface
        };
        let on_select = self.on_select;
        div()
            .id(self.id)
            .when_some(self.width, |d, w| d.w(dp(w)))
            .when(self.width.is_none(), |d| d.w_full())
            .flex()
            .flex_row()
            .items_center()
            .rounded(dp(metrics::RADIUS_LG))
            .bg(tokens.background)
            .border(dp(2.))
            .border_color(accent)
            .pl(dp(16.))
            .cursor_pointer()
            .when_some(on_select, |d, f| {
                d.on_click(move |_, window, cx| f(window, cx))
            })
            .child(div().flex_none().size(dp(16.)).rounded_full().bg(accent))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .pl(dp(16.))
                    .py(dp(8.))
                    .text_color(tokens.on_background)
                    .child(div().text_body1().child(self.title))
                    .when_some(self.subtitle, |col, text| {
                        col.child(
                            div()
                                .text_body2()
                                .text_color(tokens.on_background_secondary)
                                .child(text),
                        )
                    }),
            )
            .test_support()
    }
}
