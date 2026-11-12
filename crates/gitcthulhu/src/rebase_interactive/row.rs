//! One step row of the editor (08 §6) and the drag ghost.

use gpui_kit::component::input::Textarea;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::{RebaseDrag, RebaseInteractiveView, ids, plan};
use crate::backend::RebaseAction;
use crate::menus::MenuItem;
use crate::metrics::rebase as m;
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::DropDownMenuText;

/// The row of step `ix`: action dropdown and message field, selected highlight, drag source.
pub(super) fn render(
    view: &Entity<RebaseInteractiveView>,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let tokens = *cx.tokens();
    let (ui, editing, dragging, editor) = {
        let v = view.read(cx);
        (v.ctx.ui.clone(), v.editing, v.dragging, v.editor.clone())
    };
    let (commit, action, text, selected, actions, summary) = {
        let ui = ui.read(cx);
        let Some(plan) = ui.rebase_plan.as_ref() else {
            return div().into_any_element();
        };
        let Some(step) = plan.steps.get(ix) else {
            return div().into_any_element();
        };
        (
            step.commit,
            step.action,
            SharedString::from(plan::display_message(step).to_owned()),
            ui.selection.commit().map(|(id, _)| id) == Some(step.commit),
            plan::allowed_actions(plan, ix),
            SharedString::from(step.summary.clone()),
        )
    };
    let is_reword = action == RebaseAction::Reword;
    let is_editing = is_reword && editing == Some(commit);

    let weak = view.downgrade();
    let items: Vec<MenuItem> = actions
        .iter()
        .map(|&choice| {
            let weak = weak.clone();
            MenuItem::action(
                format!(
                    "rebase-action-{}",
                    plan::action_label(choice).to_lowercase()
                ),
                plan::action_label(choice),
                move |_, cx| {
                    weak.update(cx, |this, cx| this.choose_action(commit, choice, cx))
                        .ok();
                },
            )
        })
        .collect();
    // 08 §6 ActionDropdown: a box with a 2 dp `onBackgroundSecondary` @20 % border drawn over the dropdown (the
    // overlay has no hitbox, so hover and clicks reach the dropdown).
    let dropdown = div()
        .relative()
        .flex_none()
        .child(
            DropDownMenuText::new((ids::ACTION, ix), plan::action_label(action), items)
                .width(m::ACTION_W),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .border(dp(m::ACTION_BORDER))
                .border_color(tokens.field_border_disabled())
                .rounded(dp(metrics::RADIUS)),
        );

    let focused = is_editing
        && editor
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
    let border = if focused {
        tokens.primary_variant
    } else if is_reword {
        tokens.field_border()
    } else {
        tokens.field_border_disabled()
    };
    // The outlined field centres its text vertically (07 §10.1); body2 carries `onBackground` in every state (08
    // §6 passes the body2 style, whose colour wins over the field's disabled colour).
    let field = div()
        .id((ids::MESSAGE, ix))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .min_h(dp(m::MESSAGE_MIN_H))
        .ml(dp(m::MESSAGE_PAD_START))
        .px(dp(m::MESSAGE_PAD_H))
        .py(dp(m::MESSAGE_PAD_V))
        .border(dp(m::MESSAGE_BORDER))
        .border_color(border)
        .rounded(dp(metrics::RADIUS))
        .bg(if is_reword {
            tokens.background
        } else {
            tokens.surface
        })
        .text_body2()
        .text_color(tokens.on_background);
    let field = if is_editing {
        // Presses inside the text area must not reach the row: its click would pull the focus out of the input,
        // and its drag would reorder the steps while the user selects text.
        field
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w_full()
                    .child(Textarea::new(&editor).appearance(false).bordered(false)),
            )
            .test_support()
            .into_any_element()
    } else if is_reword {
        let weak = weak.clone();
        field
            .cursor_text()
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                weak.update(cx, |this, cx| this.begin_edit(commit, window, cx))
                    .ok();
            })
            .child(text)
            .test_support()
            .into_any_element()
    } else {
        field.child(text).test_support().into_any_element()
    };

    let click_view = weak;
    let ghost_label = SharedString::from(format!("{} {}", plan::action_label(action), summary));
    div()
        .id((ids::ROW, ix))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .px(dp(m::ROW_PAD_H))
        .py(dp(m::ROW_PAD_V))
        .when(selected, |row| row.bg(tokens.background_selected))
        .when(dragging == Some(commit), |row| row.opacity(0.5))
        .on_click(move |_, window, cx| {
            click_view
                .update(cx, |this, cx| this.row_clicked(commit, window, cx))
                .ok();
        })
        .on_drag(RebaseDrag { commit }, move |_, _, _, cx| {
            let label = ghost_label.clone();
            cx.new(|_| DragGhost { label })
        })
        .child(dropdown)
        .child(field)
        .test_support()
        .into_any_element()
}

/// What follows the pointer while a row is dragged (Gitnuro translates the row itself; the row stays in its live
/// slot here and this small card follows the pointer).
struct DragGhost {
    label: SharedString,
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        div()
            .max_w(dp(m::ACTION_W * 2.))
            .px(dp(m::ROW_PAD_H))
            .py(dp(m::ROW_PAD_V))
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.background_selected)
            .border(dp(1.))
            .border_color(tokens.popup_border())
            .opacity(0.9)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_body2()
            .text_color(tokens.on_background)
            .child(self.label.clone())
    }
}
