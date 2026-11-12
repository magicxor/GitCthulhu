//! Dropdowns (Gitnuro 07 §10.3, 09 §0.3): `DropDownMenuText` (settings selects, rebase action picker) opens a
//! popup menu; `FilterDropdown` (upstream dialog) is an outlined button with a filter field and a list.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, anchored, deferred, div, point, px,
};

use super::field::{FieldEvent, OutlinedField};
use super::{BoundsCell, observed, track_bounds, with_layer};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuItem};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

/// Width of a `DropDownMenuText` in dp.
pub const DROPDOWN_W: f32 = 180.;
/// Default width of a `FilterDropdown` in dp.
pub const FILTER_DROPDOWN_W: f32 = 240.;

/// Gitnuro `DropDownMenuText`: a 180 dp box with the current value and a chevron. Clicking opens `items` as a
/// popup menu (without icon column) at the box's bottom-left corner.
#[derive(IntoElement)]
pub struct DropDownMenuText {
    id: ElementId,
    label: SharedString,
    items: Vec<MenuItem>,
    width: f32,
}

impl DropDownMenuText {
    /// A dropdown showing `label`. `id` must be unique in the frame.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        items: Vec<MenuItem>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            items,
            width: DROPDOWN_W,
        }
    }

    /// Overrides the width in dp.
    pub fn width(mut self, width_dp: f32) -> Self {
        self.width = width_dp;
        self
    }
}

impl RenderOnce for DropDownMenuText {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let bounds: BoundsCell = Rc::new(Cell::new(Default::default()));
        let click_bounds = bounds.clone();
        let items = self.items;
        // The unstyled wrapper carries the bounds tracker, so the popup hangs from the border box.
        div()
            .relative()
            .flex_none()
            .w(dp(self.width))
            .child(track_bounds(&bounds))
            .child(
                div()
                    .id(self.id)
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    // Gitnuro's v8 h12 padding with the 2 dp border drawn over it (Compose); a GPUI border takes
                    // layout space, so the padding shrinks by the border width.
                    .py(dp(6.))
                    .px(dp(10.))
                    .rounded(dp(metrics::RADIUS))
                    .border(dp(2.))
                    .border_color(tokens.dialog_border())
                    .bg(tokens.background)
                    .cursor_pointer()
                    .hover(move |s| s.bg(with_layer(tokens.background, tokens.hover_layer())))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_body2()
                            .text_color(tokens.on_background)
                            .child(self.label),
                    )
                    .child(icon(AppIcon::ExpandMore, 24., tokens.on_background))
                    .on_click(move |_, window, cx| {
                        let anchor = click_bounds.get().bottom_left();
                        menus::open_dropdown(window, cx, anchor, items.clone(), false);
                    })
                    .test_support(),
            )
    }
}

/// Events of [`FilterDropdown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterDropdownEvent {
    /// The option at this index of the full list was chosen.
    Selected(usize),
}

/// Gitnuro `FilterDropdown`: an outlined button; the popup starts with an auto-focused filter field followed by
/// the options that contain the filter text (case-insensitive).
pub struct FilterDropdown {
    id: SharedString,
    options: Vec<SharedString>,
    selected: Option<usize>,
    open: bool,
    width: f32,
    filter: Entity<OutlinedField>,
    bounds: BoundsCell,
    _filter_changes: Subscription,
}

impl EventEmitter<FilterDropdownEvent> for FilterDropdown {}

impl FilterDropdown {
    /// A closed dropdown. `id` must be unique in the frame; the trigger is `"<id>-button"`, the popup card
    /// `"<id>-popup"` and its rows `"<id>-option-<index>"`.
    pub fn new(
        id: impl Into<SharedString>,
        options: Vec<SharedString>,
        selected: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let id: SharedString = id.into();
        let filter_id = SharedString::from(format!("{id}-filter"));
        let filter = cx.new(|cx| OutlinedField::new(filter_id, window, cx));
        // Typing in the filter re-filters the options.
        let filter_changes = cx.subscribe(&filter, |_, _, _: &FieldEvent, cx| cx.notify());
        Self {
            id,
            options,
            selected,
            open: false,
            width: FILTER_DROPDOWN_W,
            filter,
            bounds: Rc::new(Cell::new(Default::default())),
            _filter_changes: filter_changes,
        }
    }

    /// Replaces the options and the selection.
    pub fn set_options(
        &mut self,
        options: Vec<SharedString>,
        selected: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.options = options;
        self.selected = selected;
        cx.notify();
    }

    /// The selected option index.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Whether the popup is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Indices of the options that match `filter` (case-insensitive `contains`; empty matches all).
    pub fn matching(options: &[SharedString], filter: &str) -> Vec<usize> {
        let needle = filter.to_lowercase();
        options
            .iter()
            .enumerate()
            .filter(|(_, o)| needle.is_empty() || o.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.filter.update(cx, |f, cx| {
                f.set_text("", window, cx);
                f.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = Some(index);
        self.open = false;
        cx.emit(FilterDropdownEvent::Selected(index));
        cx.notify();
    }
}

impl Render for FilterDropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let label = self
            .selected
            .and_then(|i| self.options.get(i))
            .cloned()
            .unwrap_or_default();
        let trigger = div()
            .id(SharedString::from(format!("{}-button", self.id)))
            .w(dp(self.width))
            .h(dp(36.))
            .px(dp(16.))
            .py(dp(8.))
            .flex()
            .flex_row()
            .items_center()
            .rounded(dp(metrics::RADIUS))
            .border(dp(1.))
            .border_color(tokens.on_background.alpha(0.12))
            .bg(tokens.background)
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(tokens.background, tokens.hover_layer())))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_body1()
                    .text_color(tokens.on_background)
                    .child(label),
            )
            .child(icon(AppIcon::ExpandMore, 24., tokens.on_background))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx)))
            .test_support();
        let popup: Option<AnyElement> = self.open.then(|| {
            let filter_text = self.filter.read(cx).text(cx);
            let rows: Vec<AnyElement> = Self::matching(&self.options, &filter_text)
                .into_iter()
                .map(|index| {
                    let selected = self.selected == Some(index);
                    div()
                        .id(SharedString::from(format!("{}-option-{index}", self.id)))
                        .w_full()
                        .min_h(dp(48.))
                        .px(dp(16.))
                        .flex()
                        .items_center()
                        .text_body2()
                        .text_color(tokens.on_background)
                        .when(selected, |r| r.bg(tokens.background_selected))
                        .cursor_pointer()
                        .hover(move |s| s.bg(tokens.hover_layer()))
                        .on_click(cx.listener(move |this, _, _, cx| this.choose(index, cx)))
                        .child(self.options[index].clone())
                        .test_support()
                        .into_any_element()
                })
                .collect();
            let at = self.bounds.get().bottom_left();
            let card = observed(SharedString::from(format!("{}-popup", self.id)))
                .w(dp(self.width))
                .max_h(dp(320.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .rounded(dp(metrics::RADIUS))
                .bg(tokens.background)
                .border(dp(1.))
                .border_color(tokens.popup_border())
                .child(div().p(dp(4.)).child(self.filter.clone()))
                .children(rows);
            // The backdrop covers the whole window (an `absolute().size_full()` layer would only cover this
            // widget's own box); presses inside the card must not reach it, or the popup would close on mouse-down
            // and the option's click would never arrive.
            let viewport = window.viewport_size();
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .w(viewport.width)
                        .h(viewport.height)
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.open = false;
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored().position(at).snap_to_window().child(
                                div()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(card),
                            ),
                        ),
                ),
            )
            .with_priority(1)
            .into_any_element()
        });
        // The unstyled wrapper carries the bounds tracker, so the popup hangs from the trigger's border box.
        div()
            .relative()
            .flex_none()
            .w(dp(self.width))
            .child(track_bounds(&self.bounds))
            .child(trigger)
            .children(popup)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn filter_matches_case_insensitive_substrings() {
        let options: Vec<SharedString> = ["origin/main", "origin/Dev", "upstream/main"]
            .into_iter()
            .map(SharedString::from)
            .collect();
        assert_eq!(
            FilterDropdown::matching(&options, ""),
            [0, 1, 2],
            "empty filter matches all"
        );
        assert_eq!(
            FilterDropdown::matching(&options, "MAIN"),
            [0, 2],
            "case-insensitive"
        );
        assert_eq!(FilterDropdown::matching(&options, "dev"), [1], "substring");
        assert!(
            FilterDropdown::matching(&options, "zzz").is_empty(),
            "no match"
        );
    }
}
