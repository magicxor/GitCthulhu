//! The side panel (owned by G3 after G0b; 07 §6, spec 21 §4.3): the filter field and one flattened `uniform_list`
//! of section headers and ref rows (local branches, remotes with their branches, tags, stashes, submodules).
//!
//! Rows are rebuilt (pure, UI thread) when the refs, submodules or worktrees change, the filter changes or a
//! section or remote is expanded or collapsed ([`model::build_rows`]); only the visible rows are turned into
//! elements. After each paint the panel asks the tab for ahead/behind counts of the visible local branches that have
//! a tracking ref (deferred, never during render). Expansion state, the filter and the selection live in
//! [`RepoUi`](crate::tab::ui::RepoUi) / the model, never in per-row entities.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    UniformListScrollHandle, Window, div, uniform_list,
};

use crate::backend::ObjectId;
use crate::icons::{AppIcon, icon};
use crate::metrics::side_panel as m;
use crate::metrics::{self, dp};
use crate::tab::ui::{CloseableView, RepoUiEvent, SideExpansion};
use crate::tab::{RepoModelEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::BorderState;
use crate::widgets::observed;
use crate::widgets::scroll::list_scrollbar;

pub mod model;
pub mod rows;

#[cfg(test)]
mod tests;

use model::{RowInputs, SideRow, build_rows, tracking_pairs_by_branch};

/// Element ids of the side panel. Rows use domain ids, never indexes (10 §10).
pub mod ids {
    use gpui_kit::{ElementId, SharedString};

    use super::model::Section;

    /// A named part of the element `id` (the icon or text of a row).
    pub fn child(id: SharedString, part: &'static str) -> ElementId {
        (ElementId::from(id), part).into()
    }

    /// The side panel.
    pub const SIDE_PANEL: &str = "side-panel";
    /// The filter field's frame.
    pub const SIDE_FILTER: &str = "side-filter";
    /// The filter's leading search icon.
    pub const SIDE_FILTER_ICON: &str = "side-filter-icon";
    /// The box of the filter's text input.
    pub const SIDE_FILTER_TEXT: &str = "side-filter-text";
    /// The clear button of the filter (only while it has text).
    pub const SIDE_FILTER_CLEAR: &str = "side-filter-clear";
    /// The list of rows.
    pub const SIDE_LIST: &str = "side-list";

    /// A section header.
    pub fn header(section: Section) -> SharedString {
        format!("side:header:{}", section.key()).into()
    }

    /// The "+" button of a section header.
    pub fn header_add(section: Section) -> SharedString {
        format!("side:header:{}:add", section.key()).into()
    }

    /// A local branch row (`full` = `refs/heads/x`).
    pub fn branch(full: &str) -> SharedString {
        format!("side:branch:{full}").into()
    }

    /// A remote row.
    pub fn remote(name: &str) -> SharedString {
        format!("side:remote:{name}").into()
    }

    /// A remote branch row (`full` = `refs/remotes/origin/x`).
    pub fn remote_branch(full: &str) -> SharedString {
        format!("side:remote-branch:{full}").into()
    }

    /// A tag row.
    pub fn tag(full: &str) -> SharedString {
        format!("side:tag:{full}").into()
    }

    /// A stash row.
    pub fn stash(id: &str) -> SharedString {
        format!("side:stash:{id}").into()
    }

    /// A submodule row.
    pub fn submodule(path: &str) -> SharedString {
        format!("side:submodule:{path}").into()
    }
}

/// The rows built for the current inputs, with the inputs they were built from.
struct Built {
    rows: Arc<Vec<SideRow>>,
    /// Local branch index -> (local target, tracking target).
    pairs: Arc<HashMap<usize, (ObjectId, ObjectId)>>,
    generation: u64,
    filter: SharedString,
    expanded: SideExpansion,
    collapsed: HashSet<String>,
    worktrees_enabled: bool,
    valid: bool,
}

/// The side panel of a repository tab.
pub struct SidePanelView {
    ctx: TabCtx,
    input: Entity<InputState>,
    scroll: UniformListScrollHandle,
    built: Built,
    /// Bumped on every refs/submodules/worktrees change; part of the rows' cache key.
    generation: u64,
    /// Ahead/behind pairs already asked for (the tab loads each once; a failed load is not retried per frame).
    requested: HashSet<(ObjectId, ObjectId)>,
    _subscriptions: Vec<Subscription>,
}

impl SidePanelView {
    /// The side panel of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Search for branches, tags & more", window, cx);
            state
        });
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.on_filter_changed(cx),
                InputEvent::Focus => this.on_filter_focused(cx),
                _ => {}
            }),
            cx.subscribe(&ctx.model, |this, _, event: &RepoModelEvent, cx| {
                let watched = crate::tab::model::REFS
                    | crate::backend::RefreshSet::SUBMODULES
                    | crate::backend::RefreshSet::WORKTREES;
                if event.0.intersects(watched) {
                    this.generation += 1;
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &ctx.ui,
                window,
                |this, _, event: &RepoUiEvent, window, cx| {
                    this.on_ui_event(event, window, cx);
                },
            ),
        ];
        Self {
            ctx,
            input,
            scroll: UniformListScrollHandle::new(),
            built: Built {
                rows: Arc::new(Vec::new()),
                pairs: Arc::new(HashMap::new()),
                generation: 0,
                filter: SharedString::default(),
                expanded: SideExpansion::default(),
                collapsed: HashSet::new(),
                worktrees_enabled: false,
                valid: false,
            },
            generation: 1,
            requested: HashSet::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this panel.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The filter text changed: publishes it to the tab's UI state (07 §6.2), makes Escape clear it while it has
    /// text, and scrolls the list to the top.
    fn on_filter_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        self.ctx.ui.update(cx, |ui, cx| {
            if ui.side_filter != text {
                ui.side_filter = text;
                cx.emit(RepoUiEvent::TogglesChanged);
                cx.notify();
            }
            let on_top = ui.views.as_slice().last() == Some(&CloseableView::SidePaneSearch);
            if !ui.side_filter.is_empty() && !on_top {
                ui.push_view(CloseableView::SidePaneSearch, cx);
            }
        });
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Focusing the filter makes Escape clear it (07 §6.2: `SIDE_PANE_SEARCH` joins the closeable views).
    fn on_filter_focused(&mut self, cx: &mut Context<Self>) {
        self.ctx
            .ui
            .update(cx, |ui, cx| ui.push_view(CloseableView::SidePaneSearch, cx));
    }

    /// Escape (the frame) clears `side_filter` behind our back: clear the input as well.
    fn on_ui_event(&mut self, event: &RepoUiEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(event, RepoUiEvent::ViewStackChanged)
            && self.ctx.ui.read(cx).side_filter.is_empty()
            && !self.input.read(cx).value().is_empty()
        {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Clears the filter (the field's clear button).
    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.on_filter_changed(cx);
    }

    /// Rebuilds the flattened rows when any of their inputs changed.
    fn ensure_rows(&mut self, cx: &mut Context<Self>) {
        let (filter, expanded, collapsed) = {
            let ui = self.ctx.ui.read(cx);
            (
                ui.side_filter.clone(),
                ui.side_expanded.clone(),
                ui.remotes_collapsed.clone(),
            )
        };
        let worktrees_enabled = crate::worktrees::section_enabled(cx);
        let b = &self.built;
        if b.valid
            && b.generation == self.generation
            && b.filter == filter
            && b.expanded == expanded
            && b.collapsed == collapsed
            && b.worktrees_enabled == worktrees_enabled
        {
            return;
        }
        let (rows, pairs) = {
            let model = self.ctx.model.read(cx);
            let refs = model.refs.value.clone();
            let submodules = model.submodules.value.clone().unwrap_or_default();
            let worktrees = model.worktrees.value.clone().unwrap_or_default();
            let inputs = RowInputs {
                refs: refs.as_deref(),
                submodules: &submodules,
                worktrees: worktrees_enabled.then_some(&worktrees[..]),
                filter: &filter,
                expanded: &expanded,
                remotes_collapsed: &collapsed,
            };
            (
                build_rows(&inputs),
                refs.as_deref().map(tracking_pairs_by_branch),
            )
        };
        let pairs = pairs.unwrap_or_default();
        // Forget pairs that no longer exist: the tab drops their cached counts, so a pair that comes back (a branch
        // moved back by undo or reset) must be asked for again.
        if !self.requested.is_empty() {
            let live: HashSet<(ObjectId, ObjectId)> = pairs.values().copied().collect();
            self.requested.retain(|p| live.contains(p));
        }
        self.built = Built {
            rows: Arc::new(rows),
            pairs: Arc::new(pairs),
            generation: self.generation,
            filter,
            expanded,
            collapsed,
            worktrees_enabled,
            valid: true,
        };
    }

    /// Asks the tab, after this frame, for the ahead/behind counts of `pairs` that were not asked for yet.
    fn request_ahead_behind(
        &mut self,
        pairs: Vec<(ObjectId, ObjectId)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fresh: Vec<_> = pairs
            .into_iter()
            .filter(|p| self.requested.insert(*p))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let ctx = self.ctx.clone();
        window.defer(cx, move |window, cx| {
            ctx.with_tab(window, cx, move |tab, _, cx| {
                tab.request_ahead_behind(fresh, cx)
            });
        });
    }

    fn render_filter(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let focused = self
            .input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        let border = BorderState::of(focused, false, false).color(&tokens);
        let has_text = !self.input.read(cx).value().is_empty();
        let clear = has_text.then(|| {
            div()
                .id(ids::SIDE_FILTER_CLEAR)
                .flex_none()
                .pl(dp(m::FILTER_CLEAR_PAD_START))
                .pr(dp(m::FILTER_CLEAR_PAD_END))
                .cursor_pointer()
                .child(icon(AppIcon::Close, m::ICON, tokens.on_background))
                .on_click(cx.listener(|this, _, window, cx| this.clear_filter(window, cx)))
                .test_support()
        });
        div()
            .w_full()
            .flex_none()
            .pl(dp(m::FILTER_MARGIN_START))
            .child(
                observed(ids::SIDE_FILTER)
                    .w_full()
                    .h(dp(m::FILTER_H))
                    .flex()
                    .flex_row()
                    .items_center()
                    .bg(tokens.background)
                    .border(dp(metrics::FIELD_BORDER))
                    .border_color(border)
                    .rounded(dp(metrics::RADIUS))
                    .child(
                        div()
                            .flex_none()
                            .pl(dp(m::FILTER_ICON_PAD_START))
                            .pr(dp(m::FILTER_ICON_PAD_END))
                            .child(
                                div()
                                    .id(ids::SIDE_FILTER_ICON)
                                    .size(dp(m::ICON))
                                    .child(icon(
                                        AppIcon::Search,
                                        m::ICON,
                                        if has_text {
                                            tokens.on_background
                                        } else {
                                            tokens.on_background_secondary
                                        },
                                    ))
                                    .test_support(),
                            ),
                    )
                    .child(
                        div()
                            .id(ids::SIDE_FILTER_TEXT)
                            .flex_1()
                            .min_w_0()
                            .text_body2()
                            .text_color(tokens.on_background)
                            // The kit input pads itself (10 px) and uses its own 14 px text: the text starts right
                            // after the icon's 8 dp spacer and is body2 (07 §6.2: 13 sp).
                            .child(
                                Input::new(&self.input)
                                    .appearance(false)
                                    .bordered(false)
                                    .focus_bordered(false)
                                    .px_0()
                                    .text_body2(),
                            )
                            .test_support(),
                    )
                    .children(clear),
            )
    }

    /// The elements of the rows in `range` (the visible ones).
    fn render_rows(
        &mut self,
        rows: &[SideRow],
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let slice = rows.get(range).unwrap_or(&[]);
        let mut wanted = Vec::new();
        let elements = slice
            .iter()
            .map(|row| self.render_row(row, &mut wanted, cx))
            .collect();
        self.request_ahead_behind(wanted, window, cx);
        elements
    }
}

impl Render for SidePanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_rows(cx);
        let rows = self.built.rows.clone();
        let count = rows.len();
        let list = uniform_list(
            ids::SIDE_LIST,
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                this.render_rows(&rows, range, window, cx)
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        observed(ids::SIDE_PANEL)
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_filter(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mt(dp(m::LIST_TOP_PAD))
                    .relative()
                    .child(list)
                    .child(list_scrollbar("side-scroll", &self.scroll)),
            )
    }
}

/// Registers the side panel's key bindings, actions and menu extenders (G3). The panel has no shortcuts of its own:
/// Escape is the frame's `CloseLastView`, which clears the filter through the `SidePaneSearch` closeable view.
pub fn register(_cx: &mut App) {}
