//! The row of the Worktrees section (spec 21 §5.7), built exactly like the side panel's entry rows (07 §6.4): a
//! fixed-height box with hover and press layers, a 16 dp icon at x = 32, the clipped directory name from x = 56 and
//! trailing Body2 content with 16 dp padding.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use super::metrics::{MARKER_GAP, MARKER_ICON};
use super::{display_name, ids, open_in_new_tab};
use crate::backend::{HeadState, WorktreeInfo};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, worktree};
use crate::metrics::side_panel as m;
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::TooltipExt as _;

/// The checked-out branch's short name, "detached", "bare" (a bare main repository) or "unknown" (HEAD unreadable).
pub fn branch_label(info: &WorktreeInfo) -> SharedString {
    if info.bare {
        return "bare".into();
    }
    match &info.head {
        Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) => {
            crate::tab::ops::branch::short_name(name).into()
        }
        Some(HeadState::Detached { .. }) => "detached".into(),
        None => "unknown".into(),
    }
}

/// The tooltip of a row's text: the directory, and what a marker means.
fn tooltip_text(info: &WorktreeInfo) -> String {
    let mut text = info.path.display().to_string();
    if info.id.is_none() {
        text.push_str("\nMain worktree");
    }
    if info.is_current {
        text.push_str("\nOpen in this tab");
    }
    if let Some(reason) = &info.locked {
        if reason.is_empty() {
            text.push_str("\nLocked");
        } else {
            text.push_str(&format!("\nLocked: {reason}"));
        }
    }
    if info.prunable {
        text.push_str("\nThe directory is missing; Prune worktrees removes the entry");
    }
    text
}

pub(super) fn render(ctx: &TabCtx, info: &WorktreeInfo, selected: bool, cx: &App) -> AnyElement {
    let tokens = *cx.tokens();
    let height = metrics::line_heights(cx).side_item;
    let id = ids::row(&info.path);
    let role = if info.is_current {
        AppIcon::FolderOpen
    } else {
        AppIcon::Folder
    };
    let secondary = tokens.on_background_secondary;
    let layer = move |l: gpui_kit::Hsla| {
        if selected {
            tokens.background_selected.blend(l)
        } else {
            l
        }
    };

    // 21 §5.7: the trailing content carries the side panel's 16 dp horizontal padding (07 §6.5); inside it the
    // markers precede the branch label with a 4 dp gap.
    let mut trailing = div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(dp(MARKER_GAP))
        .px(dp(m::ROW_TRAILING_PAD_H));
    if info.prunable {
        trailing = trailing.child(
            div()
                .id(ids::part(id.clone(), "prunable"))
                .flex_none()
                .size(dp(MARKER_ICON))
                .child(icon(AppIcon::Warning, MARKER_ICON, tokens.error))
                .delayed_tooltip("The directory is missing; Prune worktrees removes the entry")
                .test_support(),
        );
    }
    if let Some(reason) = &info.locked {
        let text = if reason.is_empty() {
            "Locked".to_owned()
        } else {
            format!("Locked: {reason}")
        };
        trailing = trailing.child(
            div()
                .id(ids::part(id.clone(), "lock"))
                .flex_none()
                .size(dp(MARKER_ICON))
                .child(icon(AppIcon::Lock, MARKER_ICON, secondary))
                .delayed_tooltip(text)
                .test_support(),
        );
    }
    let trailing = trailing.child(
        div()
            .id(ids::part(id.clone(), "branch"))
            .flex_none()
            .whitespace_nowrap()
            .text_body2()
            .text_color(secondary)
            .child(branch_label(info))
            .test_support(),
    );

    let menu_target = worktree::row_target(ctx, info);
    let (open_path, can_open) = (info.path.clone(), !info.bare && !info.prunable);
    div()
        .id(id.clone())
        .w_full()
        .h(dp(height))
        .flex_none()
        .when(selected, |row| row.bg(tokens.background_selected))
        .hover(move |s| s.bg(layer(tokens.hover_layer())))
        .active(move |s| s.bg(layer(tokens.press_layer())))
        .on_click(move |ev, window, cx| {
            if ev.click_count() == 2 && can_open {
                open_in_new_tab(window, cx, open_path.clone());
            }
        })
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            let items = worktree::worktree_row(&menu_target, cx);
            menus::open_context_menu(window, cx, ev.position, items);
        })
        .child(
            div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .pl(dp(m::ROW_ICON_PAD_START))
                        .pr(dp(m::ROW_ICON_PAD_END))
                        .child(
                            div()
                                .id(ids::part(id.clone(), "icon"))
                                .size(dp(m::ICON))
                                .child(icon(role, m::ICON, tokens.primary_variant))
                                .test_support(),
                        ),
                )
                .child(
                    div()
                        .id(ids::part(id, "text"))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        // Long directory names end in "…" before the branch label, as other rows do.
                        .text_ellipsis()
                        .text_body2()
                        .text_color(tokens.on_background)
                        .when(info.is_current, |t| t.bold())
                        .delayed_tooltip(tooltip_text(info))
                        .child(display_name(&info.path))
                        .test_support(),
                )
                .child(trailing),
        )
        .test_support()
        .into_any_element()
}
