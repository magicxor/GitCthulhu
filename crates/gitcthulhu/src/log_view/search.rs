//! The log search bar, the full-message cache filled with `backend::log::messages` and the matcher (owned by G4 after
//! G0b; 08 §2.3, spec 21 §4.2).
//!
//! Matching runs in memory over the loaded rows: hash prefix, author and committer name and e-mail, and the full
//! message, case-insensitive. Full messages come from a cache filled lazily in the background (chunks of at most
//! `MAX_MESSAGE_IDS` ids); until a row's message is cached its summary is matched. Navigation only scrolls; it
//! never changes the selection. The bar registers `CloseableView::LogSearch`, so Escape closes it.

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, ScrollStrategy, SharedString, Styled as _,
    Subscription, TestSupportExt as _, Window, div,
};

use super::{LogView, ids};
use crate::backend::log::{MAX_MESSAGE_IDS, messages};
use crate::backend::{CancelToken, ObjectId};
use crate::icons::AppIcon;
use crate::metrics::dp;
use crate::metrics::log as m;
use crate::tab::ReadSlot;
use crate::tab::model::{ChunkedRows, LogCommit, LogRow};
use crate::tab::ui::{CloseableView, LogSearchUi, RepoUiEvent};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::IconButton;

gpui_kit::actions!(gitcthulhu_log, [LogSearchNext, LogSearchPrev]);

/// Key context of the search bar.
pub const LOG_SEARCH_CONTEXT: &str = "LogSearch";

/// The hint / floating label of the search field (08 §2.3).
pub const SEARCH_LABEL: &str = "Search by message, author name or commit ID";

/// Search state of a log view.
#[derive(Default)]
pub struct SearchState {
    /// The bar is shown.
    pub open: bool,
    input: Option<Entity<InputState>>,
    /// The last typed text, restored when the bar opens again (08 §2.3).
    last_query: SharedString,
    /// Lowercased "author, author e-mail, committer, committer e-mail, full message" per commit.
    cache: HashMap<ObjectId, Box<str>>,
    fetch: ReadSlot,
    _input: Option<Subscription>,
}

/// The lowercased text a commit is searched in.
pub fn haystack(c: &LogCommit, message: &str) -> Box<str> {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        c.author_name, c.author_email, c.committer_name, c.committer_email, message
    )
    .to_lowercase()
    .into_boxed_str()
}

fn hash_prefix(c: &LogCommit, q: &str) -> bool {
    if q.is_empty() || q.len() > 40 || !q.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    if q.len() <= c.short_id.len() {
        c.short_id.starts_with(q)
    } else {
        c.short_id.starts_with(&q[..c.short_id.len()]) && c.id.to_string().starts_with(q)
    }
}

/// Whether commit `c` matches the lowercased query `q`.
pub fn commit_matches(c: &LogCommit, q: &str, cache: &HashMap<ObjectId, Box<str>>) -> bool {
    if hash_prefix(c, q) {
        return true;
    }
    match cache.get(&c.id) {
        Some(text) => text.contains(q),
        None => [
            &c.summary,
            &c.author_name,
            &c.author_email,
            &c.committer_name,
            &c.committer_email,
        ]
        .iter()
        .any(|s| s.to_lowercase().contains(q)),
    }
}

/// The layout rows of the loaded commits matching `query` (case-insensitive), ascending; empty for a blank query.
pub fn find_matches(
    rows: &ChunkedRows<LogRow>,
    query: &str,
    cache: &HashMap<ObjectId, Box<str>>,
) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            let c = row.commit.as_ref()?;
            commit_matches(c, &q, cache).then_some(ix)
        })
        .collect()
}

/// The next 1-based match index: `delta` steps from `index`, without wrap-around (08 §2.3).
pub fn step(index: usize, total: usize, delta: isize) -> usize {
    if total == 0 {
        return 0;
    }
    let next = index as isize + delta;
    next.clamp(1, total as isize) as usize
}

impl LogView {
    /// Opens the search bar (header magnifier): restores the last text, focuses the field, registers
    /// `CloseableView::LogSearch`.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.search.input {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| InputState::new(window, cx));
                let sub = cx.subscribe_in(
                    &input,
                    window,
                    |this, _, ev: &InputEvent, window, cx| match ev {
                        InputEvent::Change => this.on_query_changed(window, cx),
                        // A single-line `Input` propagates Enter after emitting `PressEnter`, so the `LogSearch`
                        // bindings (`LogSearchNext` / `LogSearchPrev`, 21 §2.12) navigate; handling `PressEnter`
                        // too would move two matches per key press.
                        InputEvent::PressEnter { .. } => {}
                        InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    },
                );
                self.search.input = Some(input.clone());
                self.search._input = Some(sub);
                input
            }
        };
        let was_open = self.search.open;
        self.search.open = true;
        let query = self.search.last_query.clone();
        if input.read(cx).value() != query {
            input.update(cx, |state, cx| state.set_value(query.clone(), window, cx));
        }
        input.update(cx, |state, cx| state.focus(window, cx));
        if !was_open {
            self.ctx.ui.update(cx, |ui, cx| {
                ui.log_search = Some(LogSearchUi::default());
                ui.push_view(CloseableView::LogSearch, cx);
            });
        }
        self.apply_query(query, true, cx);
        self.ensure_messages(cx);
        cx.notify();
    }

    /// Closes the search bar (its close button): the same as Escape.
    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search.open {
            return;
        }
        self.search.open = false;
        self.search.fetch.cancel();
        self.ctx.ui.update(cx, |ui, cx| {
            ui.views.remove(CloseableView::LogSearch);
            ui.log_search = None;
            cx.emit(RepoUiEvent::ViewStackChanged);
            cx.notify();
        });
        self.ctx
            .with_tab(window, cx, |tab, window, cx| tab.focus(window, cx));
        cx.notify();
    }

    /// The view stack changed: Escape may have closed the search.
    pub(super) fn on_view_stack_changed(&mut self, cx: &mut Context<Self>) {
        if self.search.open && self.ctx.ui.read(cx).log_search.is_none() {
            self.search.open = false;
            self.search.fetch.cancel();
            cx.notify();
        }
    }

    fn on_query_changed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.search.input else {
            return;
        };
        let query = input.read(cx).value();
        if query == self.search.last_query
            && self
                .ctx
                .ui
                .read(cx)
                .log_search
                .as_ref()
                .is_some_and(|s| s.query == query)
        {
            return;
        }
        self.search.last_query = query.clone();
        self.apply_query(query, true, cx);
        self.ensure_messages(cx);
    }

    /// Recomputes the matches of `query`; `jump` selects the first match and scrolls to it (a new query, 08 §2.3).
    fn apply_query(&mut self, query: SharedString, jump: bool, cx: &mut Context<Self>) {
        let matches = {
            let model = self.ctx.model.read(cx);
            match model.log.value.as_ref() {
                Some(snapshot) => find_matches(&snapshot.rows, &query, &self.search.cache),
                None => Vec::new(),
            }
        };
        let previous = self.ctx.ui.read(cx).log_search.as_ref().map(|s| s.index);
        let index = if matches.is_empty() {
            0
        } else if jump {
            1
        } else {
            previous.unwrap_or(1).clamp(1, matches.len())
        };
        let first = (jump && index == 1).then(|| matches[0]);
        self.ctx.ui.update(cx, |ui, cx| {
            if let Some(search) = ui.log_search.as_mut() {
                search.query = query;
                search.matches = Arc::new(matches);
                search.index = index;
                cx.notify();
            }
        });
        if let Some(ix) = first {
            self.scroll_to_layout_row(ix, cx);
        }
    }

    /// Moves to the previous (`-1`) or next (`1`) match and scrolls to it; the selection is unchanged.
    pub fn navigate(&mut self, delta: isize, cx: &mut Context<Self>) {
        let target = self.ctx.ui.update(cx, |ui, cx| {
            let search = ui.log_search.as_mut()?;
            let index = step(search.index, search.matches.len(), delta);
            if index == 0 {
                return None;
            }
            search.index = index;
            cx.notify();
            search.matches.get(index - 1).copied()
        });
        if let Some(ix) = target {
            self.scroll_to_layout_row(ix, cx);
        }
    }

    /// Scrolls layout row `ix` into view (to the top) unless it is visible already.
    fn scroll_to_layout_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(v) = self.visible_index(ix, cx) {
            self.scroll.scroll_to_item(v, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// New rows (paging, reload) while the search is open: match them too and fetch their messages.
    pub(super) fn refresh_search_after_log(&mut self, cx: &mut Context<Self>) {
        if !self.search.open {
            return;
        }
        let query = self.search.last_query.clone();
        self.apply_query(query, false, cx);
        self.ensure_messages(cx);
    }

    /// Starts reading the full messages of loaded commits that are not cached yet.
    fn ensure_messages(&mut self, cx: &mut Context<Self>) {
        // Only a non-empty query needs full messages (21 §4.2).
        if !self.search.open
            || self.search.last_query.trim().is_empty()
            || self.search.fetch.is_running()
        {
            return;
        }
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        let missing: Vec<LogCommit> = {
            let model = self.ctx.model.read(cx);
            let Some(snapshot) = model.log.value.as_ref() else {
                return;
            };
            snapshot
                .rows
                .iter()
                .filter_map(|r| r.commit.as_ref())
                .filter(|c| !self.search.cache.contains_key(&c.id))
                .cloned()
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        self.search.fetch.start(
            cx,
            |v: &mut LogView| &mut v.search.fetch,
            move |cancel: &CancelToken| {
                let mut out = Vec::with_capacity(missing.len());
                for chunk in missing.chunks(MAX_MESSAGE_IDS) {
                    if cancel.is_cancelled() {
                        return (out, None);
                    }
                    let ids: Vec<ObjectId> = chunk.iter().map(|c| c.id).collect();
                    match messages(&repo, &ids, cancel) {
                        Ok(texts) => out.extend(
                            chunk
                                .iter()
                                .zip(texts)
                                .map(|(c, text)| (c.id, haystack(c, &text))),
                        ),
                        Err(err) => return (out, Some(err.to_string())),
                    }
                }
                (out, None)
            },
            |view, (entries, error): (Vec<(ObjectId, Box<str>)>, Option<String>), cx| {
                view.search.cache.extend(entries);
                if let Some(error) = &error {
                    tracing::warn!("reading commit messages for the log search failed: {error}");
                }
                if view.search.open {
                    let query = view.search.last_query.clone();
                    view.apply_query(query, false, cx);
                    if error.is_none() {
                        view.ensure_messages(cx);
                    }
                }
                cx.notify();
            },
        );
    }

    /// The 56 dp search bar (08 §2.3).
    pub(super) fn render_search_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *cx.tokens();
        let Some(input) = self.search.input.clone() else {
            return div().into_any_element();
        };
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let query = input.read(cx).value();
        let floating = focused || !query.is_empty();
        let (index, total) = self
            .ctx
            .ui
            .read(cx)
            .log_search
            .as_ref()
            .map_or((0, 0), |s| (s.index, s.matches.len()));
        let weak = cx.entity().downgrade();
        let (prev, next, close) = (weak.clone(), weak.clone(), weak);
        let focus_input = input.clone();
        let field = div()
            .id(ids::SEARCH_FIELD)
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus_input.update(cx, |state, cx| state.focus(window, cx));
            })
            .child(
                div()
                    .absolute()
                    .top(dp(if floating {
                        m::SEARCH_LABEL_TOP_FLOATING
                    } else {
                        m::SEARCH_LABEL_TOP
                    }))
                    .left(dp(m::SEARCH_FIELD_PAD_H))
                    .when(floating, |label| label.text_tooltip())
                    .when(!floating, |label| label.text_body1())
                    .text_color(if focused {
                        t.primary_variant
                    } else {
                        t.on_background_secondary
                    })
                    .whitespace_nowrap()
                    .child(SEARCH_LABEL),
            )
            .child(
                div()
                    .absolute()
                    .left(dp(m::SEARCH_FIELD_PAD_H))
                    .right(dp(m::SEARCH_FIELD_PAD_H))
                    .top(dp(m::SEARCH_INPUT_TOP))
                    .text_body1()
                    .text_color(t.on_background)
                    .child(
                        Input::new(&input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
            .test_support();
        div()
            .id(ids::SEARCH_BAR)
            .key_context(LOG_SEARCH_CONTEXT)
            .flex_none()
            .w_full()
            .h(dp(m::SEARCH_BAR_H))
            .flex()
            .flex_row()
            .items_center()
            .bg(t.background)
            // The whole bar is the filled text field (the arrows and close are its trailing icons), so the
            // indicator line spans the full width (08 §2.3).
            .rounded_t(dp(crate::metrics::RADIUS))
            .border_b(dp(if focused {
                m::SEARCH_INDICATOR_FOCUSED
            } else {
                m::SEARCH_INDICATOR
            }))
            .border_color(if focused {
                t.primary_variant
            } else {
                t.on_background.alpha(0.42)
            })
            .on_action(cx.listener(|this, _: &LogSearchNext, _, cx| this.navigate(1, cx)))
            .on_action(cx.listener(|this, _: &LogSearchPrev, _, cx| this.navigate(-1, cx)))
            .child(field)
            .child(
                div()
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .when(!query.is_empty(), |row| {
                        row.child(
                            div()
                                .id(ids::SEARCH_COUNTER)
                                .text_body1()
                                .text_color(t.on_background_secondary)
                                .whitespace_nowrap()
                                .child(format!("{index}/{total}"))
                                .test_support(),
                        )
                    })
                    .child(
                        IconButton::new(ids::SEARCH_PREV, AppIcon::ArrowUp)
                            .icon_size(m::SEARCH_ICON)
                            .on_click(move |_, cx| {
                                prev.update(cx, |this, cx| this.navigate(-1, cx)).ok();
                            }),
                    )
                    .child(
                        IconButton::new(ids::SEARCH_NEXT, AppIcon::ExpandMore)
                            .icon_size(m::SEARCH_ICON)
                            .on_click(move |_, cx| {
                                next.update(cx, |this, cx| this.navigate(1, cx)).ok();
                            }),
                    )
                    .child(
                        div().pr(dp(m::SEARCH_CLOSE_PAD_END)).child(
                            IconButton::new(ids::SEARCH_CLOSE, AppIcon::Close)
                                .icon_size(m::SEARCH_ICON)
                                .on_click(move |window, cx| {
                                    close
                                        .update(cx, |this, cx| this.close_search(window, cx))
                                        .ok();
                                }),
                        ),
                    ),
            )
            .test_support()
            .into_any_element()
    }
}
