//! The Quick actions dialog (owned by G2; 09 §3.13, 07 §5.4): a search field over a short list of actions. Up and
//! Down move the selection, Enter or a click runs it, Escape closes the dialog.

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, div,
};

use super::{DialogFrame, DialogSpec};
use crate::backend::RefreshSet;
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{ObservedExt as _, with_layer};

/// Element ids of the dialog.
pub mod ids {
    /// The dialog content (680 x 400).
    pub const QUICK_ACTIONS: &str = "quick-actions";
    /// The search field.
    pub const FIELD: &str = "quick-actions-field";
    /// The list of rows.
    pub const LIST: &str = "quick-actions-list";
    /// A row; the id is `(ROW, index in the filtered list)`.
    pub const ROW: &str = "quick-action-row";
    /// The icon box of a row (24 dp icon with 16 dp padding); the id is `(ROW_ICON, index in the filtered list)`.
    pub const ROW_ICON: &str = "quick-action-row-icon";
}

/// Width of the content (07 §5.4).
pub const CONTENT_W: f32 = 680.;
/// Height of the content (07 §5.4).
pub const CONTENT_H: f32 = 400.;
/// Height of a row: a 24 dp icon with 16 dp padding on every side (07 §5.4).
pub const ROW_H: f32 = 56.;
/// Padding around a row's icon (07 §5.4).
pub const ROW_ICON_PAD: f32 = 16.;
/// Space between the field and the list (09 §3.13).
pub const FIELD_GAP: f32 = 16.;
/// The field's hint (09 §3.13).
pub const HINT: &str = "Search for an action or press ESC to close the dialog";
/// Key context of the dialog content.
pub const CONTEXT: &str = "QuickActions";

gpui_kit::actions!(
    gitcthulhu_quick_actions,
    [
        /// Moves the selection one row up.
        SelectPrevious,
        /// Moves the selection one row down.
        SelectNext
    ]
);

/// One entry of the dialog.
pub struct QuickAction {
    /// Stable id (logs, tests).
    pub id: &'static str,
    /// The row's icon.
    pub icon: AppIcon,
    /// The row's text; the filter matches against it.
    pub title: &'static str,
    run: fn(&TabCtx, &mut Window, &mut App),
}

/// The actions in display order (07 §5.4).
pub const ACTIONS: [QuickAction; 4] = [
    QuickAction {
        id: "open-in-file-manager",
        icon: AppIcon::Code,
        title: "Open repository in file manager",
        run: open_in_file_manager,
    },
    QuickAction {
        id: "clone",
        icon: AppIcon::Download,
        title: "Clone new repository",
        run: clone_repository,
    },
    QuickAction {
        id: "refresh",
        icon: AppIcon::Refresh,
        title: "Refresh repository data",
        run: refresh,
    },
    QuickAction {
        id: "signoff",
        icon: AppIcon::Sign,
        title: "Signoff config",
        run: signoff_config,
    },
];

fn open_in_file_manager(ctx: &TabCtx, _window: &mut Window, cx: &mut App) {
    let path = ctx.tab.upgrade().map(|tab| tab.read(cx).path().clone());
    if let Some(path) = path {
        crate::toolbar::platform::open_in_file_manager(cx, &path);
    }
}

fn clone_repository(_ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    super::clone::open(window, cx);
}

fn refresh(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    ctx.with_tab(window, cx, |tab, _, cx| tab.reload(RefreshSet::ALL, cx));
}

fn signoff_config(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    super::signoff::open(window, cx, ctx.clone());
}

/// The indices into [`ACTIONS`] whose title contains `filter` (case-insensitive), in display order.
pub fn matching(filter: &str) -> Vec<usize> {
    let needle = filter.to_lowercase();
    ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, action)| action.title.to_lowercase().contains(&needle))
        .map(|(ix, _)| ix)
        .collect()
}

/// The content of the dialog.
struct QuickActionsView {
    ctx: TabCtx,
    field: Entity<OutlinedField>,
    /// Index into the filtered list.
    selected: usize,
    focused: bool,
    _subscription: Subscription,
}

impl QuickActionsView {
    fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| OutlinedField::new(ids::FIELD, window, cx).hint(HINT, window, cx));
        let subscription = cx.subscribe_in(
            &field,
            window,
            |this, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => {
                    this.selected = 0;
                    cx.notify();
                }
                FieldEvent::Enter => this.run_selected(window, cx),
            },
        );
        Self {
            ctx,
            field,
            selected: 0,
            focused: false,
            _subscription: subscription,
        }
    }

    fn filtered(&self, cx: &App) -> Vec<usize> {
        matching(&self.field.read(cx).text(cx))
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.filtered(cx).len();
        if count == 0 {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        cx.notify();
    }

    fn run_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(self.selected, window, cx);
    }

    /// Runs the row `ix` of the filtered list and closes the dialog.
    fn run(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&action) = self.filtered(cx).get(ix) else {
            return;
        };
        window.close_dialog(cx);
        (ACTIONS[action].run)(&self.ctx, window, cx);
    }
}

impl Render for QuickActionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focused = true;
            let field = self.field.clone();
            // The kit dialog focuses its own handle after the layer is first drawn; focus the field after that.
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    field.update(cx, |field, cx| field.focus(window, cx));
                });
            });
        }
        let tokens = *cx.tokens();
        let rows = self
            .filtered(cx)
            .into_iter()
            .enumerate()
            .map(|(ix, action)| {
                let action = &ACTIONS[action];
                // The hover layer lies on top of the row's own background, so a hovered selected row stays selected.
                let base = if ix == self.selected {
                    tokens.background_selected
                } else {
                    tokens.surface
                };
                div()
                    .id((ids::ROW, ix))
                    .flex_none()
                    .w_full()
                    .h(dp(ROW_H))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(dp(metrics::RADIUS))
                    .cursor_pointer()
                    .when(ix == self.selected, |row| {
                        row.bg(tokens.background_selected)
                    })
                    .hover(move |style| style.bg(with_layer(base, tokens.hover_layer())))
                    .on_click(cx.listener(move |this, _, window, cx| this.run(ix, window, cx)))
                    .child(
                        div()
                            .id((ids::ROW_ICON, ix))
                            .flex_none()
                            .p(dp(ROW_ICON_PAD))
                            .child(icon(
                                action.icon,
                                metrics::TOOLBAR_ICON,
                                tokens.on_background,
                            ))
                            .test_support(),
                    )
                    .child(
                        div()
                            .text_body1()
                            .text_color(tokens.on_background)
                            .child(action.title),
                    )
                    .test_support()
            });
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .w(dp(CONTENT_W))
            .h(dp(CONTENT_H))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .pb(dp(FIELD_GAP))
                    .child(self.field.clone()),
            )
            .child(
                div()
                    .id(ids::LIST)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .observed(ids::QUICK_ACTIONS)
    }
}

/// Opens the Quick actions dialog of the tab of `tab`.
pub fn open(window: &mut Window, cx: &mut App, tab: TabCtx) {
    let view = cx.new(|cx| QuickActionsView::new(tab, window, cx));
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: CONTENT_W + 2. * metrics::DIALOG_PAD,
            est_height: CONTENT_H + 2. * metrics::DIALOG_PAD,
        },
        move |_, _| view.clone().into_any_element(),
    );
}

/// Binds Up and Down for the dialog, also while the search field has the focus (a focused kit `Input` would
/// otherwise take them).
pub fn register(cx: &mut App) {
    let input_context = format!("{CONTEXT} > Input");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(input_context.as_str())),
        KeyBinding::new("down", SelectNext, Some(input_context.as_str())),
    ]);
}

#[cfg(test)]
mod tests;
