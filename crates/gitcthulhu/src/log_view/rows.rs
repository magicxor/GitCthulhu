//! Commit rows and the uncommitted row of the log (owned by G4 after G0b; 08 §2.7, §2.8, §2.9 nodes).
//!
//! A row is one `div` of the list's row height: the graph cell (selection background, the node element at
//! `x = 30 × (lane + 1) − 15 − hscroll`, clipped to the graph column) and the message area starting at the graph
//! width (search match bar, chips, message, date). The lines are painted by the decoration (`graph.rs`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, AppContext as _, ClickEvent, Context, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::chips::ChipColors;
use super::graph::{LANE_W, NODE_HALF};
use super::{LogView, ids};
use crate::backend::{GitTime, NodeKind, ObjectId};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuKind, MenuTarget};
use crate::metrics::log as m;
use crate::metrics::{dp, dpx};
use crate::tab::model::{GraphCell, LogCommit, LogRow, LogSnapshot};
use crate::tab::ui::Selection;
use crate::theme::{TextStyleExt as _, Tokens};
use crate::widgets::avatar::{Avatar, AvatarShape};
use crate::widgets::date::{DateFormat, format_date, local_offset_minutes};
use crate::widgets::{InstantTooltip, with_layer};

/// Which row is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selected {
    None,
    Uncommitted,
    /// The commit of a commit-based selection (commit, branch, tag or stash).
    Commit(ObjectId),
}

/// The search state rows need.
#[derive(Clone)]
pub struct SearchFrame {
    /// Layout rows of the matches, ascending.
    pub matches: Arc<Vec<usize>>,
    /// The query is not empty: matches get the bar, other rows are dimmed (an empty query dims nothing, fix of
    /// 08 §9.4).
    pub active: bool,
}

impl SearchFrame {
    /// Whether layout row `ix` matches.
    pub fn matches(&self, ix: usize) -> bool {
        self.matches.binary_search(&ix).is_ok()
    }
}

/// What the uncommitted row shows.
#[derive(Clone, Copy)]
pub struct UncommittedInfo {
    pub label: &'static str,
    /// Modified, added, deleted, conflicting.
    pub counts: [u32; 4],
}

/// Everything the rows of one frame render from (built once per frame in `render`).
pub struct Frame {
    pub snapshot: Arc<LogSnapshot>,
    /// 1 while the uncommitted row is hidden.
    pub offset: usize,
    pub show: bool,
    /// Graph column width in dp.
    pub graph_w: f32,
    pub hscroll: Pixels,
    /// Row height in dp.
    pub row_h: f32,
    pub tokens: Tokens,
    pub lanes: [Hsla; 6],
    pub remote_chip: Hsla,
    pub selected: Selected,
    /// `Some` while the search bar is open.
    pub search: Option<SearchFrame>,
    pub uncommitted: UncommittedInfo,
    pub date_format: Rc<DateFormat>,
    pub now: GitTime,
}

impl Frame {
    fn lane(&self, lane: u16) -> Hsla {
        self.lanes[usize::from(lane) % self.lanes.len()]
    }
}

/// Whether a commit row of the log is a stash (selected as `Selection::Stash` when listed).
pub fn is_stash_row(row: &LogRow) -> bool {
    row.commit.as_ref().is_some_and(|c| c.is_stash) || row.graph.kind == NodeKind::Stash
}

/// The current time.
pub fn now() -> GitTime {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    GitTime {
        seconds,
        offset_minutes: local_offset_minutes(seconds),
    }
}

fn local_day(seconds: i64, offset_minutes: i32) -> i64 {
    (seconds + i64::from(offset_minutes) * 60).div_euclid(86_400)
}

/// Row dates formatted once per local day (the text depends only on the day; it changes when "today" or the date
/// settings change).
#[derive(Default)]
pub struct DateCache {
    key: Option<(i64, DateFormat)>,
    days: HashMap<i64, SharedString>,
}

impl DateCache {
    /// Drops the cache when the day or the format changed.
    pub fn begin_frame(&mut self, now: GitTime, format: &DateFormat) {
        let today = local_day(now.seconds, now.offset_minutes);
        let fresh = self
            .key
            .as_ref()
            .is_some_and(|(day, f)| *day == today && f == format);
        if !fresh {
            self.days.clear();
            self.key = Some((today, format.clone()));
        }
    }

    /// The date text of `time` (08 §2.7.2: relative, no time).
    pub fn get(&mut self, time: GitTime, now: GitTime, format: &DateFormat) -> SharedString {
        let offset = local_offset_minutes(time.seconds);
        let day = local_day(time.seconds, offset);
        self.days
            .entry(day)
            .or_insert_with(|| format_date(time, now, offset, format, false).into())
            .clone()
    }
}

/// The date tooltip of a row: never "Today" / "Yesterday", always with the time (08 §2.7.2:
/// `allowRelative = false, showTime = true`).
pub fn date_tooltip(
    time: GitTime,
    now: GitTime,
    offset_minutes: i32,
    format: &DateFormat,
) -> String {
    let absolute = DateFormat {
        relative: false,
        ..format.clone()
    };
    format_date(time, now, offset_minutes, &absolute, true)
}

/// An instant tooltip whose text is built only when it shows.
fn lazy_tooltip<E: gpui_kit::StatefulInteractiveElement>(
    el: E,
    text: impl Fn() -> String + 'static,
) -> E {
    el.tooltip(move |_, cx| AnyView::from(cx.new(|_| InstantTooltip::new(text(), None))))
        .tooltip_show_delay(Duration::ZERO)
}

impl LogView {
    /// The element of layout row `ix`.
    pub(super) fn render_row(
        &mut self,
        ix: usize,
        f: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = f.snapshot.rows.get(ix) else {
            return div().into_any_element();
        };
        match &row.commit {
            None => self.render_uncommitted(ix, row, f, window, cx),
            Some(commit) => self.render_commit(ix, row, commit, f, window, cx),
        }
    }

    /// The x of a node's left edge inside the graph cell (pixels, after the horizontal scroll).
    fn node_left(lane: u16, f: &Frame, window: &Window) -> Pixels {
        dpx(LANE_W * (f32::from(lane) + 1.) - NODE_HALF, window) - f.hscroll
    }

    fn row_shell(&self, ix: usize, selected: bool, f: &Frame) -> gpui_kit::Stateful<gpui_kit::Div> {
        let t = f.tokens;
        let bg = if selected {
            t.background_selected
        } else {
            t.background
        };
        div()
            .id((ids::LOG_ROW, ix))
            .relative()
            .w_full()
            .h(dp(f.row_h))
            .overflow_hidden()
            .bg(bg)
            .hover(move |s| s.bg(with_layer(bg, t.hover_layer())))
    }

    fn graph_cell(f: &Frame) -> gpui_kit::Div {
        div()
            .absolute()
            .top_0()
            .left_0()
            .h_full()
            .w(dp(f.graph_w))
            .overflow_hidden()
    }

    fn render_uncommitted(
        &mut self,
        ix: usize,
        row: &LogRow,
        f: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = f.tokens;
        let lane = f.lane(row.graph.node_lane);
        let node = div()
            .id((ids::NODE, ix))
            .absolute()
            .left(Self::node_left(row.graph.node_lane, f, window))
            .top(dp((f.row_h - m::NODE) / 2.))
            .size(dp(m::NODE))
            .rounded_full()
            .bg(lane)
            .test_support();
        let summary_icons = [
            (AppIcon::Edit, t.modified_file),
            (AppIcon::Add, t.add_file),
            (AppIcon::Delete, t.deleted_file),
            (AppIcon::Warning, t.conflicting_file),
        ];
        let entries = f
            .uncommitted
            .counts
            .iter()
            .zip(summary_icons)
            .enumerate()
            .filter(|(_, (count, _))| **count > 0)
            .map(|(k, (count, (role, color)))| {
                div()
                    .id((ids::SUMMARY, k))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(dp(m::SUMMARY_ENTRY_PAD_H))
                    .child(
                        div()
                            .text_body2()
                            .text_color(t.on_background)
                            .child(count.to_string()),
                    )
                    .child(icon(role, m::SUMMARY_ICON, color))
                    .test_support()
            });
        let message = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(dp(f.graph_w))
            .right_0()
            .p(dp(m::UNCOMMITTED_PAD))
            .flex()
            .flex_row()
            .items_center()
            .child(
                div()
                    .id(ids::UNCOMMITTED_LABEL)
                    .flex_none()
                    .pl(dp(m::UNCOMMITTED_LABEL_PAD))
                    .text_body2()
                    .italic_text()
                    .text_color(t.on_background)
                    .whitespace_nowrap()
                    .child(f.uncommitted.label)
                    .test_support(),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(dp(m::SUMMARY_PAD_H))
                    .children(entries),
            );
        self.row_shell(ix, f.selected == Selected::Uncommitted, f)
            .child(Self::graph_cell(f).child(node))
            .child(message)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.ctx
                    .ui
                    .update(cx, |ui, cx| ui.select(Selection::UncommittedChanges, cx));
                // Up / Down move the selection from here on.
                window.focus(&this.focus, cx);
            }))
            .test_support()
            .into_any_element()
    }

    fn render_node(
        &self,
        ix: usize,
        g: &GraphCell,
        commit: &LogCommit,
        f: &Frame,
        window: &Window,
    ) -> AnyElement {
        let t = f.tokens;
        let lane = f.lane(g.node_lane);
        let base = div()
            .id((ids::NODE, ix))
            .absolute()
            .left(Self::node_left(g.node_lane, f, window))
            .top(dp((f.row_h - m::NODE) / 2.))
            .size(dp(m::NODE));
        let ring = |rounded_square: bool| {
            let ring = div()
                .absolute()
                .inset_0()
                .border(dp(m::STROKE))
                .border_color(lane);
            if rounded_square {
                ring.rounded(dp(m::MERGE_NODE_RADIUS))
            } else {
                ring.rounded_full()
            }
        };
        match g.kind {
            NodeKind::Stash => base
                .rounded_full()
                .bg(t.background)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(AppIcon::Stash, m::STASH_ICON, lane))
                .child(ring(false))
                .test_support()
                .into_any_element(),
            NodeKind::ShallowBoundary => {
                let (name, email) = (commit.author_name.clone(), commit.author_email.clone());
                lazy_tooltip(
                    base.rounded_full().bg(t.background).child(ring(false)),
                    move || format!("{name} <{email}>"),
                )
                .test_support()
                .into_any_element()
            }
            NodeKind::Uncommitted => base
                .rounded_full()
                .bg(lane)
                .test_support()
                .into_any_element(),
            NodeKind::Commit | NodeKind::Merge | NodeKind::Root => {
                let merge = g.kind == NodeKind::Merge || commit.parent_count > 1;
                let shape = if merge {
                    AvatarShape::RoundedSquare
                } else {
                    AvatarShape::Circle
                };
                let (name, email) = (commit.author_name.clone(), commit.author_email.clone());
                // Half a stroke smaller on each side, so the avatar's edge lies under the ring.
                let avatar = Avatar::new(name.clone(), email.clone(), m::NODE - m::STROKE)
                    .shape(shape)
                    .fallback_color(lane);
                let node = if merge {
                    base.rounded(dp(m::MERGE_NODE_RADIUS))
                } else {
                    base.rounded_full()
                };
                lazy_tooltip(
                    node.flex()
                        .items_center()
                        .justify_center()
                        .child(avatar)
                        .child(ring(merge)),
                    move || format!("{name} <{email}>"),
                )
                .test_support()
                .into_any_element()
            }
        }
    }

    fn render_commit(
        &mut self,
        ix: usize,
        row: &LogRow,
        commit: &LogCommit,
        f: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = f.tokens;
        let id = commit.id;
        let is_stash = is_stash_row(row);
        let selected = f.selected == Selected::Commit(id);
        let matched = f.search.as_ref().is_some_and(|s| s.active && s.matches(ix));
        let dimmed = f.search.as_ref().is_some_and(|s| s.active) && !matched;
        let date = self.dates.get(commit.committer_time, f.now, &f.date_format);
        let chips = if is_stash {
            None
        } else {
            let list = self.chips.get(&id);
            let colors = ChipColors {
                lane: f.lane(row.graph.node_lane),
                remote: f.remote_chip,
                tokens: t,
            };
            Some(self.render_chips(ix, list, colors, cx))
        };
        let (time, format) = (commit.committer_time, f.date_format.clone());
        let date_el = lazy_tooltip(
            div()
                .id((ids::DATE, ix))
                .flex_none()
                .px(dp(m::DATE_PAD_H))
                .text_caption()
                .text_color(t.on_background_secondary)
                .whitespace_nowrap()
                .child(date),
            move || date_tooltip(time, now(), local_offset_minutes(time.seconds), &format),
        )
        .test_support();
        let message = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(dp(f.graph_w))
            .right_0()
            .pr(dp(m::ROW_PAD_END))
            .flex()
            .flex_row()
            .items_center()
            .when(matched, |area| {
                area.child(
                    div()
                        .id((ids::MATCH_BAR, ix))
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(dp(m::MATCH_BAR_START))
                        .w(dp(m::MATCH_BAR_W))
                        .bg(t.secondary)
                        .test_support(),
                )
            })
            .when_some(chips, |area, chips| {
                area.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .pl(dp(m::CHIPS_PAD_START))
                        .children(chips),
                )
            })
            // Deviation from 08 §2.7.2: a stash row keeps the (empty) chip container's start padding, so its message
            // lines up with every other row at graphWidth + 24 instead of 16 dp further left.
            .when(is_stash, |area| {
                area.child(div().flex_none().pl(dp(m::CHIPS_PAD_START)))
            })
            .child(
                div()
                    .id((ids::MESSAGE, ix))
                    .flex_1()
                    .min_w_0()
                    .pl(dp(m::MESSAGE_PAD_START))
                    .text_body2()
                    .text_color(if dimmed {
                        t.on_background_secondary
                    } else {
                        t.on_background
                    })
                    .truncate()
                    .child(commit.summary.clone())
                    .test_support(),
            )
            .child(date_el);
        let node = self.render_node(ix, &row.graph, commit, f, window);
        self.row_shell(ix, selected, f)
            .child(Self::graph_cell(f).child(node))
            .child(message)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_commit(id, is_stash, ix, cx);
                // Up / Down move the selection from here on.
                window.focus(&this.focus, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.open_row_menu(id, is_stash, ev.position, window, cx);
                }),
            )
            .test_support()
            .into_any_element()
    }

    /// Click on a commit row: selects it (stashes as `Selection::Stash` when listed); while searching, the match
    /// index follows a clicked match (08 §2.3).
    pub(super) fn select_commit(
        &mut self,
        id: ObjectId,
        is_stash: bool,
        ix: usize,
        cx: &mut Context<Self>,
    ) {
        let stash_index = is_stash
            .then(|| {
                let model = self.ctx.model.read(cx);
                let refs = model.refs.value.as_ref()?;
                refs.stashes.iter().find(|s| s.id == id).map(|s| s.index)
            })
            .flatten();
        let selection = match stash_index {
            Some(index) => Selection::Stash { id, index },
            None => Selection::Commit {
                id,
                is_stash,
                scroll_to: false,
            },
        };
        self.ctx.ui.update(cx, |ui, cx| {
            ui.select(selection, cx);
            if let Some(search) = ui.log_search.as_mut()
                && let Ok(pos) = search.matches.binary_search(&ix)
            {
                search.index = pos + 1;
                cx.notify();
            }
        });
    }

    /// Right press on a commit row: the commit menu, or the stash menu on stash rows (08 §2.13).
    pub(super) fn open_row_menu(
        &mut self,
        id: ObjectId,
        is_stash: bool,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_head = self
            .ctx
            .model
            .read(cx)
            .state
            .value
            .as_ref()
            .is_some_and(|s| {
                matches!(
                    s.head,
                    crate::backend::HeadState::Branch { target, .. }
                        | crate::backend::HeadState::Detached { target } if target == id
                )
            });
        let target = MenuTarget {
            kind: if is_stash {
                MenuKind::Stash
            } else {
                MenuKind::Commit
            },
            tab: self.ctx.clone(),
            branch: None,
            upstream: None,
            commit: Some(id),
            path: None,
            is_current: is_head,
            checked_out_elsewhere: false,
        };
        let items = if is_stash {
            menus::refs::stash(&target, cx)
        } else {
            menus::log::commit(&target, cx)
        };
        menus::open_context_menu(window, cx, at, items);
    }
}
