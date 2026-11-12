//! The Undo split button of the toolbar: Gitnuro's `ExtendedMenuButton` geometry (07 §5.1.3: 64 x 56, a 48 dp main
//! part and a 16 dp chevron strip) with a chevron that opens the history popover instead of a menu. The shared
//! widget cannot do that: its chevron only opens a `MenuItem` dropdown.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::ids;
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{Handler, TooltipExt as _};

/// The Undo button.
#[derive(IntoElement)]
pub struct UndoSplit {
    /// The main part reacts to clicks.
    pub enabled: bool,
    /// The chevron reacts to clicks (the history is available).
    pub chevron_enabled: bool,
    /// Tooltip of the main part.
    pub tooltip: SharedString,
    /// Shortcut hint of the tooltip.
    pub hint: Option<SharedString>,
    /// Click on the main part.
    pub on_main: Handler,
    /// Click on the chevron.
    pub on_chevron: Handler,
}

impl RenderOnce for UndoSplit {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let color = |enabled: bool| {
            if enabled {
                tokens.on_background
            } else {
                tokens.disabled_content()
            }
        };
        let enabled = self.enabled;
        let chevron_enabled = self.chevron_enabled;
        let on_main = self.on_main;
        let on_chevron = self.on_chevron;
        let hint = if enabled { self.hint } else { None };
        let main_color = color(enabled);
        let main = div()
            .id((ElementId::from(ids::TOOLBAR_UNDO), "main"))
            .flex_1()
            .h_full()
            .cursor_pointer()
            .when(enabled, |m| {
                m.hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .on_click(move |_, window, cx| on_main(window, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .size_full()
                    .child(icon(AppIcon::UndoOp, metrics::TOOLBAR_ICON, main_color))
                    .child(
                        div()
                            .text_caption()
                            .text_color(main_color)
                            .whitespace_nowrap()
                            .child("Undo"),
                    ),
            )
            .instant_tooltip(self.tooltip, hint)
            .test_support();
        let chevron = div()
            .id((ElementId::from(ids::TOOLBAR_UNDO), "chevron"))
            .flex_none()
            .w(dp(metrics::EXT_CHEVRON_W))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(icon(
                AppIcon::ExpandMore,
                metrics::ICON_SM,
                color(chevron_enabled),
            ))
            .when(chevron_enabled, |c| {
                c.hover(move |s| s.bg(tokens.hover_layer()))
                    .active(move |s| s.bg(tokens.press_layer()))
                    .on_click(move |_, window, cx| on_chevron(window, cx))
            })
            .test_support();
        div().mr(dp(metrics::TOOLBAR_ITEM_GAP)).flex_none().child(
            div()
                .id(ids::TOOLBAR_UNDO)
                .w(dp(metrics::EXT_MENU_BUTTON_W))
                .h(dp(metrics::MENU_BUTTON))
                .flex()
                .flex_row()
                .rounded(dp(metrics::RADIUS))
                .overflow_hidden()
                .bg(tokens.surface)
                .child(main)
                .child(chevron)
                .test_support(),
        )
    }
}
