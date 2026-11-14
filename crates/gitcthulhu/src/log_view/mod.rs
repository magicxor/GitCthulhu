//! The log view (owned by G4 after G0b; 08 §2, spec 21 §4.2): search bar, header, a bare `uniform_list` of commit
//! rows with the one-pass graph decoration, the graph divider (header and full height), the horizontal graph
//! scrollbar, the "Scroll to top" pill and the loading bar.
//!
//! Data comes only from `RepoModel` / `RepoUi` (layout row 0 is always the synthetic uncommitted row; while it is
//! hidden visible index `v` = layout row `v + 1`). Rows render from `ChunkedRows` by index; there are no per-row
//! entities. Operations start from event handlers through `TabCtx::with_tab`. After a click on a row the list has the
//! keyboard focus: Up / Down select the previous / next row like a click does.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, DragMoveEvent, FocusHandle, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement as _, Pixels,
    Render, ScrollDelta, ScrollStrategy, ScrollWheelEvent, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, UniformListScrollHandle, Window, div, point,
    px, uniform_list,
};

use crate::backend::{InProgress, ObjectId, RefreshSet, Status};
use crate::icons::{AppIcon, icon};
use crate::metrics::log as m;
use crate::metrics::{self, BASE_REM, dp, dpx};
use crate::tab::model::RepoModel;
use crate::tab::ui::{RepoUi, Selection};
use crate::tab::{RepoModelEvent, RepoTabEvent, RepoUiEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::scroll::{HorizontalThumbBar, list_scrollbar};
use crate::widgets::split::DragGhost;
use crate::widgets::{HeaderBar34, IconButton, LinearProgress, observed};

pub mod chips;
pub mod graph;
pub mod paging;
pub mod rows;
pub mod search;

#[cfg(test)]
mod tests;

use chips::ChipIndex;
use graph::{GraphDecoration, LANE_W};
use rows::{DateCache, Frame, Selected, UncommittedInfo};

/// Key context of the list area (Up / Down move the selection).
pub const LOG_LIST_CONTEXT: &str = "LogList";

gpui_kit::actions!(
    gitcthulhu_log,
    [
        /// Selects the previous row of the log (Up).
        SelectPreviousCommit,
        /// Selects the next row of the log (Down).
        SelectNextCommit,
    ]
);

/// Element ids of the log view.
pub mod ids {
    use gpui_kit::ElementId;

    /// The log view.
    pub const LOG_VIEW: &str = "log-view";
    /// A log row: `(LOG_ROW, layout row index)`.
    pub const LOG_ROW: &str = "log-row";
    /// The 34 dp header.
    pub const HEADER: &str = "log-header";
    /// The header's "Graph" label (width = graph column).
    pub const HEADER_GRAPH: &str = "log-header-graph";
    /// The header's graph divider handle.
    pub const HEADER_DIVIDER: &str = "log-header-divider";
    /// The header's "Message" label.
    pub const HEADER_MESSAGE: &str = "log-header-message";
    /// The header's search button.
    pub const SEARCH_BUTTON: &str = "log-search-button";
    /// The list area below the header.
    pub const LIST_AREA: &str = "log-list-area";
    /// The `uniform_list` of rows.
    pub const LIST: &str = "log-list";
    /// The vertical scrollbar of the list.
    pub const LIST_SCROLLBAR: &str = "log-list-scrollbar";
    /// The full-height graph divider over the list.
    pub const LIST_DIVIDER: &str = "log-list-divider";
    /// The horizontal graph scrollbar.
    pub const HSCROLL: &str = "log-hscroll";
    /// The "Scroll to top" pill.
    pub const SCROLL_TOP: &str = "log-scroll-top";
    /// The loading bar.
    pub const PROGRESS: &str = "log-progress";
    /// The node of a row: `(NODE, layout row)`.
    pub const NODE: &str = "log-node";
    /// The message text of a row: `(MESSAGE, layout row)`.
    pub const MESSAGE: &str = "log-message";
    /// The date of a row: `(DATE, layout row)`.
    pub const DATE: &str = "log-date";
    /// The search match bar of a row: `(MATCH_BAR, layout row)`.
    pub const MATCH_BAR: &str = "log-match-bar";
    /// The label of the uncommitted row.
    pub const UNCOMMITTED_LABEL: &str = "log-uncommitted-label";
    /// A status summary entry of the uncommitted row: `(SUMMARY, 0..4)` = modified, added, deleted, conflicting.
    pub const SUMMARY: &str = "log-summary";
    /// The search bar.
    pub const SEARCH_BAR: &str = "log-search-bar";
    /// The search field.
    pub const SEARCH_FIELD: &str = "log-search-field";
    /// The "{i}/{n}" counter.
    pub const SEARCH_COUNTER: &str = "log-search-counter";
    /// Previous match.
    pub const SEARCH_PREV: &str = "log-search-prev";
    /// Next match.
    pub const SEARCH_NEXT: &str = "log-search-next";
    /// Close the search.
    pub const SEARCH_CLOSE: &str = "log-search-close";
    /// A chip: see [`chip`].
    pub const CHIP: &str = "log-chip";
    /// The tracking cloud of a grouped chip: see [`chip_cloud`].
    pub const CHIP_CLOUD: &str = "log-chip-cloud";

    const CHIP_STRIDE: usize = 64;

    /// Chip `k` (display order) of layout row `row`.
    pub fn chip(row: usize, k: usize) -> ElementId {
        (CHIP, row * CHIP_STRIDE + k.min(CHIP_STRIDE - 1)).into()
    }

    /// The tracking cloud of chip `k` of layout row `row`.
    pub fn chip_cloud(row: usize, k: usize) -> ElementId {
        (CHIP_CLOUD, row * CHIP_STRIDE + k.min(CHIP_STRIDE - 1)).into()
    }
}

/// Width of the graph column for a drag padding on top of `default_w` (08 §2.2, see [`default_graph_width`]):
/// `max(100, default_w + padding)`.
pub fn graph_width_from(default_w: f32, padding: f32) -> f32 {
    (default_w + padding).max(m::GRAPH_MIN_W)
}

/// The padding accumulator of a column whose default width is `default_w`, clamped so that dragging below the
/// minimum width leaves no dead zone (fix of 08 §9.3).
pub fn clamp_padding_from(default_w: f32, padding: f32) -> f32 {
    padding.max(m::GRAPH_MIN_W - default_w)
}

/// The default graph width (21 §6 #38): 120 dp, or the width that shows every lane
/// (`graph_real_width(120, max_lane)`) when that is at most [`metrics::PANE_MAX_FRACTION`] of the log pane's width
/// `pane_w` (dp, `None` before the pane was laid out). A wider graph keeps 120 dp and scrolls horizontally.
pub fn default_graph_width(max_lane: Option<u16>, pane_w: Option<f32>) -> f32 {
    let fitted = graph_real_width(m::GRAPH_DEFAULT_W, max_lane);
    match pane_w {
        Some(pane) if fitted <= pane * metrics::PANE_MAX_FRACTION => fitted,
        _ => m::GRAPH_DEFAULT_W,
    }
}

/// The scrollable width of the graph (08 §2.2): `max(graph_w, (max_lane + 2) × 30)`, `max_lane` = 2 while no commit
/// is loaded. Never shrinks after paging (the snapshot's `max_lane` never shrinks).
pub fn graph_real_width(graph_w: f32, max_lane: Option<u16>) -> f32 {
    let lanes = max_lane.unwrap_or(m::MIN_GRAPH_LANES) + m::MARGIN_GRAPH_LANES;
    graph_w.max(f32::from(lanes) * LANE_W)
}

/// Drag payload of the graph dividers.
struct GraphDrag;

/// The log of a repository tab.
pub struct LogView {
    ctx: TabCtx,
    scroll: UniformListScrollHandle,
    /// Keyboard focus of the list (taken by a click on a row); a zero-size target, so it is not an ancestor of the
    /// search field.
    focus: FocusHandle,
    /// Horizontal graph scroll in pixels (fix of 08 §9.2: never dp).
    hscroll: Pixels,
    /// The largest horizontal scroll of the last frame.
    hscroll_max: Pixels,
    /// Divider drag: pointer x at the press and the padding then.
    drag: Option<(Pixels, f32)>,
    paging: paging::Paging,
    search: search::SearchState,
    chips: ChipIndex,
    dates: DateCache,
    /// What the rows of the current frame render from.
    frame: Option<Rc<Frame>>,
    /// The newest commit of the last log snapshot (`None` inside = no commit; outer `None` = no log seen yet).
    first_commit: Option<Option<ObjectId>>,
    /// Whether the uncommitted row was shown at the last check.
    shown_uncommitted: Option<bool>,
    /// The first visible commit and its layout row (the scroll anchor across reloads).
    anchor: Option<(ObjectId, usize)>,
    /// Layout row height of the last frame, in pixels.
    row_px: Pixels,
    /// Width of the log pane in pixels as of the last paint (0 = not laid out yet).
    pane_w: Rc<std::cell::Cell<Pixels>>,
    /// The default graph width of the last frame in dp (the drag clamp depends on it).
    graph_default: f32,
    _subscriptions: Vec<Subscription>,
}

impl LogView {
    /// The log of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&ctx.model, |this, _, ev: &RepoModelEvent, cx| {
                if ev.0.intersects(RefreshSet::STATUS | RefreshSet::REPO_STATE) {
                    this.on_uncommitted_visibility(cx);
                }
                if ev.0.contains(RefreshSet::LOG) {
                    this.on_log_changed(cx);
                }
            }),
            cx.subscribe(&ctx.ui, |this, _, ev: &RepoUiEvent, cx| match ev {
                RepoUiEvent::SelectionChanged => this.on_selection_changed(cx),
                RepoUiEvent::ViewStackChanged => this.on_view_stack_changed(cx),
                _ => {}
            }),
            cx.subscribe(&ctx.events, |this, _, ev: &RepoTabEvent, cx| match ev {
                RepoTabEvent::ScrollToCommit { id } => this.scroll_to_commit(*id, cx),
                RepoTabEvent::LogReloaded { generation } => this.on_log_reloaded(*generation, cx),
                _ => {}
            }),
        ];
        Self {
            ctx,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            hscroll: px(0.),
            hscroll_max: px(0.),
            drag: None,
            paging: paging::Paging::default(),
            search: search::SearchState::default(),
            chips: ChipIndex::default(),
            dates: DateCache::default(),
            frame: None,
            first_commit: None,
            shown_uncommitted: None,
            anchor: None,
            row_px: px(metrics::SPACED.log_row),
            pane_w: Rc::default(),
            graph_default: m::GRAPH_DEFAULT_W,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this log.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The horizontal graph scroll in pixels.
    pub fn hscroll(&self) -> Pixels {
        self.hscroll
    }

    /// The scroll handle of the row list.
    pub fn scroll_handle(&self) -> &UniformListScrollHandle {
        &self.scroll
    }

    /// The first visible visible-index of the list (from the last layout).
    pub fn first_visible(&self) -> usize {
        let y = -self.scroll.0.borrow().base_handle.offset().y;
        if self.row_px <= px(0.) || y <= px(0.) {
            return 0;
        }
        (y / self.row_px).floor() as usize
    }

    /// Scrolls the graph column horizontally to `value` pixels (clamped to the scroll range of the last frame).
    pub fn set_hscroll(&mut self, value: Pixels, cx: &mut Context<Self>) {
        let next = value.clamp(px(0.), self.hscroll_max.max(px(0.)));
        if next != self.hscroll {
            self.hscroll = next;
            cx.notify();
        }
    }

    fn visible_offset(model: &RepoModel) -> usize {
        usize::from(!model.show_uncommitted_row())
    }

    /// The visible index of layout row `ix` (`None` for the hidden uncommitted row).
    fn visible_index(&self, ix: usize, cx: &App) -> Option<usize> {
        ix.checked_sub(Self::visible_offset(self.ctx.model.read(cx)))
    }

    /// The layout row of commit `id` among the loaded rows.
    fn layout_row_of(&self, id: ObjectId, cx: &App) -> Option<usize> {
        let model = self.ctx.model.read(cx);
        let snapshot = model.log.value.as_ref()?;
        snapshot
            .rows
            .position(|r| r.commit.as_ref().is_some_and(|c| c.id == id))
    }

    /// Scrolls so that commit `id` is visible (at the top), only if it is not visible yet and it is loaded (fix of
    /// 08 §9.5: visibility check and the uncommitted-row offset).
    pub fn scroll_to_commit(&mut self, id: ObjectId, cx: &mut Context<Self>) {
        let Some(ix) = self.layout_row_of(id, cx) else {
            return;
        };
        if let Some(v) = self.visible_index(ix, cx) {
            self.scroll.scroll_to_item(v, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// Scrolls to the first row (the pill, 08 §2.10).
    pub fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Up / Down: selects the previous / next row like a click and scrolls it into view. At either end of the loaded
    /// rows the selection stays put (scrolling to the last rows loads the next page); a selection that is not in the
    /// list moves to the first row.
    fn move_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        let (target, offset, commit) = {
            let model = self.ctx.model.read(cx);
            let Some(snapshot) = model.log.value.as_ref() else {
                return;
            };
            let offset = Self::visible_offset(model);
            let from = match &self.ctx.ui.read(cx).selection {
                Selection::UncommittedChanges => (offset == 0).then_some(0),
                other => other.commit().and_then(|(id, _)| {
                    snapshot
                        .rows
                        .position(|r| r.commit.as_ref().is_some_and(|c| c.id == id))
                }),
            };
            let target = match from {
                Some(from) if forward => from + 1,
                Some(from) if from > offset => from - 1,
                Some(_) => return,
                None => offset,
            };
            let Some(row) = snapshot.rows.get(target) else {
                return;
            };
            let commit = row.commit.as_ref().map(|c| (c.id, rows::is_stash_row(row)));
            (target, offset, commit)
        };
        match commit {
            Some((id, is_stash)) => self.select_commit(id, is_stash, target, cx),
            None => self
                .ctx
                .ui
                .update(cx, |ui, cx| ui.select(Selection::UncommittedChanges, cx)),
        }
        self.scroll
            .scroll_to_item(target - offset, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn on_selection_changed(&mut self, cx: &mut Context<Self>) {
        let target = match &self.ctx.ui.read(cx).selection {
            Selection::Commit {
                id,
                scroll_to: true,
                ..
            } => Some(*id),
            Selection::Branch { target, .. } | Selection::Tag { target, .. } => Some(*target),
            Selection::Stash { id, .. } => Some(*id),
            _ => None,
        };
        if let Some(id) = target {
            self.scroll_to_commit(id, cx);
        }
        cx.notify();
    }

    /// The uncommitted row appeared or vanished: keep the rows in view where they are; at the very top the new
    /// row becomes visible (08 §2.10).
    fn on_uncommitted_visibility(&mut self, cx: &mut Context<Self>) {
        let show = self.ctx.model.read(cx).show_uncommitted_row();
        let before = self.shown_uncommitted.replace(show);
        if before.is_none() || before == Some(show) {
            return;
        }
        let handle = self.scroll.0.borrow().base_handle.clone();
        let offset = handle.offset();
        if offset.y < px(0.) {
            let y = if show {
                offset.y - self.row_px
            } else {
                (offset.y + self.row_px).min(px(0.))
            };
            handle.set_offset(point(offset.x, y));
        }
        cx.notify();
    }

    fn on_log_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_search_after_log(cx);
        cx.notify();
    }

    /// A new snapshot generation: a new newest commit scrolls to the top (08 §2.10); otherwise the first visible
    /// commit keeps its place (20 §3.5).
    fn on_log_reloaded(&mut self, _generation: u64, cx: &mut Context<Self>) {
        self.paging.reset();
        let (first, anchor_row) = {
            let model = self.ctx.model.read(cx);
            let Some(snapshot) = model.log.value.as_ref() else {
                return;
            };
            let first = snapshot
                .rows
                .iter()
                .find_map(|r| r.commit.as_ref().map(|c| c.id));
            let anchor_row = self.anchor.and_then(|(id, old)| {
                let new = snapshot
                    .rows
                    .position(|r| r.commit.as_ref().is_some_and(|c| c.id == id))?;
                Some((old, new))
            });
            (first, anchor_row)
        };
        let previous = self.first_commit.replace(first);
        match previous {
            Some(previous) if previous != first => self.scroll_to_top(cx),
            Some(_) => {
                if let Some((old, new)) = anchor_row
                    && old != new
                {
                    let handle = self.scroll.0.borrow().base_handle.clone();
                    let offset = handle.offset();
                    let delta = self.row_px * (new as f32 - old as f32);
                    handle.set_offset(point(offset.x, (offset.y - delta).min(px(0.))));
                }
            }
            None => {}
        }
        cx.notify();
    }

    fn on_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        // Horizontal wheel / Shift+wheel (Windows reports it as x) moves the graph, × 10 for line deltas (08 §2.6).
        let dx = match ev.delta {
            ScrollDelta::Lines(l) => px(l.x * m::HSCROLL_MULTIPLIER),
            ScrollDelta::Pixels(p) => p.x,
        };
        if dx != px(0.) {
            self.set_hscroll(self.hscroll - dx, cx);
        }
    }

    fn on_divider_press(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let padding = self.ctx.ui.read(cx).graph_padding;
        self.drag = Some((
            ev.position.x,
            clamp_padding_from(self.graph_default, padding),
        ));
    }

    fn on_divider_drag(&mut self, x: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let Some((press, start)) = self.drag else {
            return;
        };
        let scale = window.rem_size().as_f32() / BASE_REM;
        let moved = (x - press).as_f32() / scale.max(f32::EPSILON);
        let padding = clamp_padding_from(self.graph_default, start + moved);
        self.ctx.ui.update(cx, |ui, cx| {
            if ui.graph_padding != padding {
                ui.graph_padding = padding;
                cx.notify();
            }
        });
    }

    fn divider(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let line = cx.tokens().popup_border();
        div()
            .id(id)
            .flex_none()
            .w(dp(m::DIVIDER_W))
            .h_full()
            .flex()
            .justify_center()
            .cursor_col_resize()
            .child(div().w(dp(m::DIVIDER_LINE_W)).h_full().bg(line))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.on_divider_press(ev, cx)),
            )
            .on_drag(GraphDrag, |_, _, _, cx| cx.new(|_| DragGhost))
            .on_drag_move::<GraphDrag>(cx.listener(
                |this, ev: &DragMoveEvent<GraphDrag>, window, cx| {
                    this.on_divider_drag(ev.event.position.x, window, cx)
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None),
            )
    }

    /// An invisible element that records the log pane's width and repaints the view when it changes (the default
    /// graph width depends on it).
    fn pane_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cell = self.pane_w.clone();
        let view = cx.entity().downgrade();
        gpui_kit::canvas(
            move |bounds, window, cx| {
                if cell.replace(bounds.size.width) != bounds.size.width {
                    window.defer(cx, move |_, cx| {
                        view.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Records the scroll anchor and requests the next page when few rows remain below the viewport.
    fn after_visible(
        &mut self,
        range: Range<usize>,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.anchor = (range.start..range.end).find_map(|v| {
            let ix = v + frame.offset;
            let row = frame.snapshot.rows.get(ix)?;
            row.commit.as_ref().map(|c| (c.id, ix))
        });
        // The blank padding rows after the last commit are not loaded rows.
        let last_layout = (range.end + frame.offset).min(frame.snapshot.rows.len());
        if paging::needs_page(
            frame.snapshot.rows.len(),
            last_layout,
            frame.snapshot.exhausted,
        ) && self.paging.can_request(frame.snapshot.generation)
        {
            cx.defer_in(window, move |this, _, cx| {
                this.request_page(last_layout, cx)
            });
        }
    }

    fn header(&self, graph_w: f32, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = *cx.tokens();
        let weak = cx.entity().downgrade();
        HeaderBar34::new()
            .id(ids::HEADER)
            .child(
                observed(ids::HEADER_GRAPH)
                    .flex_none()
                    .w(dp(graph_w))
                    .h_full()
                    .flex()
                    .items_center()
                    .pl(dp(m::HEADER_LABEL_PAD))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_body2()
                    .text_color(t.on_background)
                    .child("Graph"),
            )
            .child(self.divider(ids::HEADER_DIVIDER, cx).test_support())
            .child(
                observed(ids::HEADER_MESSAGE)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .pl(dp(m::HEADER_LABEL_PAD))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_body2()
                    .text_color(t.on_background)
                    .child("Message"),
            )
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .pr(dp(m::HEADER_SEARCH_PAD_END))
                    .child(
                        IconButton::new(ids::SEARCH_BUTTON, AppIcon::Search)
                            .icon_size(metrics::ICON_MD)
                            .on_click(move |window, cx| {
                                weak.update(cx, |this, cx| this.open_search(window, cx))
                                    .ok();
                            }),
                    ),
            )
            .into_any_element()
    }

    fn scroll_pill(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let t = *cx.tokens();
        div()
            .absolute()
            .top(dp(m::PILL_TOP))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id(ids::SCROLL_TOP)
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded_full()
                    .bg(t.primary)
                    .py(dp(m::PILL_PAD_V))
                    .px(dp(m::PILL_PAD_H))
                    .cursor_pointer()
                    .child(icon(AppIcon::ScrollTop, m::PILL_ICON, t.on_primary))
                    .child(
                        div()
                            .pl(dp(m::PILL_GAP))
                            .text_body2()
                            .text_color(t.on_primary)
                            .whitespace_nowrap()
                            .child("Scroll to top"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.scroll_to_top(cx)))
                    .test_support(),
            )
            .into_any_element()
    }
}

/// The number of blank rows that make up LOG_BOTTOM_PADDING (80 dp, 08 §2.1) after the last of `count` rows of
/// `row_h` dp. A `uniform_list` has equal-height items only, so the padding is whole rows; it also keeps the
/// padding out of the list's own bounds, where it would hide the bottom 80 dp of rows and of the graph.
pub fn bottom_padding_rows(count: usize, row_h: f32) -> usize {
    if count == 0 || row_h <= 0. {
        return 0;
    }
    (m::BOTTOM_PADDING / row_h).ceil() as usize
}

/// The uncommitted row's label for the operation in progress (08 §2.8).
pub fn uncommitted_label(state: Option<InProgress>) -> &'static str {
    match state {
        Some(
            InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase,
        ) => "Pending changes to rebase",
        Some(InProgress::Merge) => "Pending changes to merge",
        Some(InProgress::CherryPick | InProgress::CherryPickSequence) => {
            "Pending changes to cherry-pick"
        }
        Some(InProgress::Revert | InProgress::RevertSequence) => "Pending changes to revert",
        _ => "Uncommitted changes",
    }
}

/// The uncommitted row's summary counts over staged + unstaged entries: modified, added (incl. untracked),
/// deleted, conflicting; renames are not counted (08 §2.8).
pub fn summary_counts(status: Option<&Status>) -> [u32; 4] {
    let Some(status) = status else {
        return [0; 4];
    };
    let (s, u) = (status.counts.staged, status.counts.unstaged);
    [
        s.modified + u.modified,
        s.added + u.added + s.untracked + u.untracked,
        s.deleted + u.deleted,
        s.conflicted + u.conflicted,
    ]
}

impl Render for LogView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (t, lanes, remote_chip) = {
            let theme = cx.theme_g();
            (theme.tokens, theme.lanes, theme.remote_chip)
        };
        let settings = crate::settings::get_or_default(cx);
        let row_h = metrics::LineHeights::of(settings.lines_height).log_row;
        self.row_px = dpx(row_h, window);
        let (snapshot, loading, show, refs, status, in_progress) = {
            let model = self.ctx.model.read(cx);
            (
                model.log.value.clone(),
                model.log.loading,
                model.show_uncommitted_row(),
                model.refs.value.clone(),
                model.status.value.clone(),
                model.state.value.as_ref().and_then(|s| s.in_progress),
            )
        };
        if self.shown_uncommitted.is_none() {
            self.shown_uncommitted = Some(show);
        }
        if self.first_commit.is_none()
            && let Some(snapshot) = &snapshot
        {
            self.first_commit = Some(
                snapshot
                    .rows
                    .iter()
                    .find_map(|r| r.commit.as_ref().map(|c| c.id)),
            );
        }
        let (selection, padding, search_ui) = {
            let ui: &RepoUi = self.ctx.ui.read(cx);
            (
                ui.selection.clone(),
                ui.graph_padding,
                ui.log_search.clone(),
            )
        };
        self.chips
            .sync(refs.as_ref(), settings.group_branches_with_remotes);
        let scale = window.rem_size().as_f32() / BASE_REM;
        let has_commits = snapshot.as_ref().is_some_and(|s| s.rows.len() > 1);
        let max_lane = has_commits.then(|| snapshot.as_ref().map_or(0, |s| s.max_lane));
        let pane_w = self.pane_w.get().as_f32();
        let default_w = default_graph_width(max_lane, (pane_w > 0.).then(|| pane_w / scale));
        self.graph_default = default_w;
        let graph_w = graph_width_from(default_w, clamp_padding_from(default_w, padding));
        let real_w = graph_real_width(graph_w, max_lane);
        self.hscroll_max = px((real_w - graph_w).max(0.) * scale);
        self.hscroll = self.hscroll.clamp(px(0.), self.hscroll_max);
        let offset = usize::from(!show);
        let selected = match selection {
            Selection::UncommittedChanges => Selected::Uncommitted,
            other => other
                .commit()
                .map_or(Selected::None, |(id, _)| Selected::Commit(id)),
        };
        let now = rows::now();
        let date_format = Rc::new(crate::widgets::date::DateFormat::from_settings(&settings));
        self.dates.begin_frame(now, &date_format);
        let search_frame = self.search.open.then(|| rows::SearchFrame {
            matches: search_ui
                .as_ref()
                .map(|s| s.matches.clone())
                .unwrap_or_default(),
            active: search_ui.as_ref().is_some_and(|s| !s.query.is_empty()),
        });
        let count = snapshot
            .as_ref()
            .map_or(0, |s| s.rows.len().saturating_sub(offset));
        let pad_rows = bottom_padding_rows(count, row_h);
        self.frame = snapshot.clone().map(|snapshot| {
            Rc::new(Frame {
                snapshot,
                offset,
                show,
                graph_w,
                hscroll: self.hscroll,
                row_h,
                tokens: t,
                lanes,
                remote_chip,
                selected,
                search: search_frame,
                uncommitted: UncommittedInfo {
                    label: uncommitted_label(in_progress),
                    counts: summary_counts(status.as_deref()),
                },
                date_format,
                now,
            })
        });
        let busy = loading || self.paging.is_running();
        let show_pill = self.first_visible() > 0;
        let search_bar = self.search.open.then(|| self.render_search_bar(window, cx));
        let header = self.header(graph_w, cx);

        let list = snapshot.map(|snapshot| {
            let mut list = uniform_list(
                ids::LIST,
                count + pad_rows,
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    let Some(frame) = this.frame.clone() else {
                        return Vec::new();
                    };
                    let items = range
                        .clone()
                        .map(|v| {
                            if v >= count {
                                // LOG_BOTTOM_PADDING (08 §2.1): blank rows after the last commit.
                                div().h(dp(frame.row_h)).into_any_element()
                            } else {
                                this.render_row(v + frame.offset, &frame, window, cx)
                            }
                        })
                        .collect::<Vec<_>>();
                    this.after_visible(range, &frame, window, cx);
                    items
                }),
            )
            .track_scroll(&self.scroll)
            .with_decoration(GraphDecoration {
                snapshot,
                offset,
                show_uncommitted: show,
                graph_w: dpx(graph_w, window),
                hscroll: self.hscroll,
                lanes,
                recorder: None,
            })
            .size_full();
            // A horizontal wheel moves the graph, never the list (`UniformList` has no builder method for this).
            list.style().restrict_scroll_to_axis = Some(true);
            list
        });

        let weak = cx.entity().downgrade();
        let hbar =
            HorizontalThumbBar::new(ids::HSCROLL, real_w, graph_w, self.hscroll.as_f32() / scale)
                .on_scroll(move |offset_dp, window, cx| {
                    let scale = window.rem_size().as_f32() / BASE_REM;
                    weak.update(cx, |this, cx| this.set_hscroll(px(offset_dp * scale), cx))
                        .ok();
                });

        let list_area = div()
            .id(ids::LIST_AREA)
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .key_context(LOG_LIST_CONTEXT)
            .on_action(
                cx.listener(|this, _: &SelectPreviousCommit, _, cx| this.move_selection(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectNextCommit, _, cx| this.move_selection(true, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|this, ev: &ScrollWheelEvent, _, cx| this.on_wheel(ev, cx)),
            )
            .child(div().track_focus(&self.focus).size_0())
            .children(list)
            .child(list_scrollbar(ids::LIST_SCROLLBAR, &self.scroll))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(dp(graph_w))
                    .child(self.divider(ids::LIST_DIVIDER, cx).test_support()),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .bottom_0()
                    .w(dp(graph_w))
                    .pl(dp(m::HSCROLL_INSET))
                    .pb(dp(m::HSCROLL_INSET))
                    .child(hbar),
            )
            .when(show_pill, |area| area.child(self.scroll_pill(cx)))
            .test_support();

        observed(ids::LOG_VIEW)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(t.background)
            .child(self.pane_tracker(cx))
            .children(search_bar)
            .child(header)
            .child(list_area)
            .when(busy, |view| {
                view.child(
                    observed(ids::PROGRESS)
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .child(LinearProgress::new().color(t.primary)),
                )
            })
            .into_any_element()
    }
}

/// Registers the log's key bindings: search navigation in the `LogSearch` context, Up / Down in
/// [`LOG_LIST_CONTEXT`].
pub fn register(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousCommit, Some(LOG_LIST_CONTEXT)),
        KeyBinding::new("down", SelectNextCommit, Some(LOG_LIST_CONTEXT)),
        KeyBinding::new(
            "enter",
            search::LogSearchNext,
            Some(search::LOG_SEARCH_CONTEXT),
        ),
        KeyBinding::new(
            "shift-enter",
            search::LogSearchPrev,
            Some(search::LOG_SEARCH_CONTEXT),
        ),
    ]);
}

/// Starts editing an interactive rebase onto `base` (commit menu "Rebase interactive", 08 §2.13): the rebase
/// editor's entry point reads the plan in the background (newest request wins, empty ranges are a toast).
pub fn start_rebase_interactive(ctx: &TabCtx, base: ObjectId, cx: &mut App) {
    crate::rebase_interactive::open(ctx, Some(base), cx);
}
