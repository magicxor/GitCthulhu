//! The recent repositories list (07 §4.4), used by the welcome page and the toolbar's Open popup (G2): a filter
//! field above a virtualised list of `UiState::recent_repositories`.
//!
//! Keyboard: Up/Down move the highlight (also while the filter field has focus), Enter opens the highlighted
//! repository, Esc clears the filter (an Esc with an empty filter is passed on and reported as
//! [`RecentReposEvent::Exit`]). The highlight follows the mouse only while the filter field has focus, as in Gitnuro.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    UniformListScrollHandle, Window, div, uniform_list,
};

use super::{RECENT_CONTEXT, RecentDown, RecentUp, dims, ids};
use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::settings::state;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::FieldEvent;
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{OutlinedField, TooltipExt as _, with_layer};

/// Called with the repository the user picked.
pub type OnOpen = Rc<dyn Fn(PathBuf, &mut Window, &mut App)>;

/// Events of [`RecentRepos`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecentReposEvent {
    /// Escape was pressed while the filter was empty (a hosting popup may close).
    Exit,
}

/// Indices of the entries whose full path contains `query` (case-insensitive); all of them for an empty query.
pub fn filter_recent(entries: &[PathBuf], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, path)| {
            query.is_empty() || path.to_string_lossy().to_lowercase().contains(&query)
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// The directory name of a repository (its last path segment, the whole path for a root).
pub fn repository_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The parent directory of `path`, with the home directory written as `~`.
pub fn parent_display(path: &Path, home: Option<&Path>) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    if let Some(home) = home
        && let Ok(rest) = parent.strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
        };
    }
    parent.display().to_string()
}

/// The filterable list of `UiState::recent_repositories`.
pub struct RecentRepos {
    on_open: OnOpen,
    field: Entity<OutlinedField>,
    entries: Vec<PathBuf>,
    /// Indices into `entries` that match the filter.
    shown: Vec<usize>,
    filter: String,
    /// Highlighted row of `shown`.
    focused: usize,
    scroll: UniformListScrollHandle,
    home: Option<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RecentReposEvent> for RecentRepos {}

impl RecentRepos {
    /// A list that calls `on_open` with the picked repository.
    pub fn new(on_open: OnOpen, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| {
            OutlinedField::new(ids::RECENT_FILTER, window, cx).hint(
                "Search for recent repositories",
                window,
                cx,
            )
        });
        let entries = state::get(cx).recent_repositories.clone();
        let subscriptions = vec![
            cx.subscribe_in(
                &field,
                window,
                |this, field, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => {
                        let text = field.read(cx).text(cx).to_string();
                        this.set_filter(text, cx);
                    }
                    FieldEvent::Enter => this.open_focused(window, cx),
                },
            ),
            // Every UI-state save (tab selection, window bounds) lands here: only a changed list refilters.
            cx.observe_global::<state::UiStateGlobal>(|this, cx| {
                if state::get(cx).recent_repositories != this.entries {
                    this.entries = state::get(cx).recent_repositories.clone();
                    this.refilter(cx);
                }
            }),
        ];
        let shown = filter_recent(&entries, "");
        Self {
            on_open,
            field,
            entries,
            shown,
            filter: String::new(),
            focused: 0,
            scroll: UniformListScrollHandle::new(),
            home: dirs::home_dir(),
            _subscriptions: subscriptions,
        }
    }

    /// The callback of a picked repository.
    pub fn on_open(&self) -> &OnOpen {
        &self.on_open
    }

    /// Whether any repository is remembered at all (the filter does not matter).
    pub fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    /// The repositories currently listed (the filter applied), in order.
    pub fn shown(&self) -> Vec<PathBuf> {
        self.shown
            .iter()
            .map(|&ix| self.entries[ix].clone())
            .collect()
    }

    /// The highlighted row of [`RecentRepos::shown`].
    pub fn focused_row(&self) -> usize {
        self.focused
    }

    /// Moves keyboard focus into the filter field (nothing to focus while the list is empty).
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        if self.has_entries() {
            self.field.update(cx, |f, cx| f.focus(window, cx));
        }
    }

    fn set_filter(&mut self, text: String, cx: &mut Context<Self>) {
        self.filter = text;
        self.focused = 0;
        self.refilter(cx);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.shown = filter_recent(&self.entries, &self.filter);
        self.focused = self.focused.min(self.shown.len().saturating_sub(1));
        cx.notify();
    }

    /// Whether keyboard focus is in the filter field.
    pub fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_has_focus(window, cx)
    }

    fn search_has_focus(&self, window: &Window, cx: &App) -> bool {
        self.field
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    }

    fn move_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.shown.is_empty() {
            return;
        }
        let last = self.shown.len() - 1;
        self.focused = self.focused.saturating_add_signed(delta).min(last);
        self.scroll
            .scroll_to_item(self.focused, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn open_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_has_focus(window, cx) {
            return;
        }
        let Some(&ix) = self.shown.get(self.focused) else {
            return;
        };
        let path = self.entries[ix].clone();
        (self.on_open)(path, window, cx);
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.field.update(cx, |f, cx| f.set_text("", window, cx));
        self.set_filter(String::new(), cx);
    }

    fn on_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter.is_empty() {
            cx.emit(RecentReposEvent::Exit);
            cx.propagate();
        } else {
            self.clear_filter(window, cx);
        }
    }

    fn rows(
        &self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui_kit::AnyElement> {
        let tokens = *cx.tokens();
        let search_focused = self.search_has_focus(window, cx);
        let (home, on_open) = (self.home.clone(), self.on_open.clone());
        range
            .filter_map(|v| {
                let path = self.entries.get(*self.shown.get(v)?)?.clone();
                let name = SharedString::from(repository_name(&path));
                let parent = SharedString::from(parent_display(&path, home.as_deref()));
                let full_parent = path
                    .parent()
                    .map(|p| SharedString::from(p.display().to_string()))
                    .unwrap_or_default();
                let group = SharedString::from(format!("recent-row-{v}"));
                let (open_path, remove_path, on_open) = (path.clone(), path, on_open.clone());
                // The highlighted row keeps its colour under the hover and press layers.
                let highlighted = search_focused && v == self.focused;
                let (hover, press) = if highlighted {
                    (
                        with_layer(tokens.background_selected, tokens.hover_layer()),
                        with_layer(tokens.background_selected, tokens.press_layer()),
                    )
                } else {
                    (tokens.hover_layer(), tokens.press_layer())
                };
                Some(
                    div()
                        .id((ids::RECENT_ROW, v))
                        .group(group.clone())
                        .flex_none()
                        .w_full()
                        .h(dp(dims::RECENT_ROW_H))
                        .px(dp(dims::RECENT_ROW_PAD_H))
                        .flex()
                        .flex_row()
                        .items_center()
                        .rounded(dp(metrics::RADIUS))
                        .cursor_pointer()
                        .when(highlighted, |row| row.bg(tokens.background_selected))
                        .hover(move |s| s.bg(hover))
                        .active(move |s| s.bg(press))
                        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                            if *hovered && this.focused != v && this.search_has_focus(window, cx) {
                                this.focused = v;
                                cx.notify();
                            }
                        }))
                        .on_click(move |_, window, cx| on_open(open_path.clone(), window, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(dp(dims::RECENT_ROW_GAP))
                                .child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_body2()
                                        .medium()
                                        .text_color(tokens.primary_variant)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .id(("recent-parent", v))
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_body2()
                                        .text_color(tokens.on_background_secondary)
                                        .instant_tooltip(full_parent, None)
                                        .child(parent),
                                ),
                        )
                        .child(
                            div()
                                .id((ids::RECENT_REMOVE, v))
                                .flex_none()
                                .size(dp(dims::RECENT_REMOVE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .opacity(0.)
                                .group_hover(group, |s| s.opacity(1.))
                                .hover(move |s| s.bg(tokens.hover_layer()))
                                .active(move |s| s.bg(tokens.press_layer()))
                                .delayed_tooltip("Remove repository from recent")
                                // Keep the focus in the filter field (a focusable host would take it).
                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                    window.prevent_default()
                                })
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    state::update(cx, |s| s.remove_recent(&remove_path));
                                })
                                .child(icon(
                                    AppIcon::Close,
                                    metrics::ICON_MD,
                                    tokens.on_background_secondary,
                                ))
                                .test_support(),
                        )
                        .test_support(),
                )
            })
            .map(|row| row.into_any_element())
            .collect()
    }
}

impl Render for RecentRepos {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let root = div()
            .id(ids::RECENT_REPOS)
            .size_full()
            .flex()
            .flex_col()
            .key_context(RECENT_CONTEXT)
            .on_action(cx.listener(|this, _: &RecentUp, _, cx| this.move_focus(-1, cx)))
            .on_action(cx.listener(|this, _: &RecentDown, _, cx| this.move_focus(1, cx)))
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::input::Escape, window, cx| {
                    this.on_escape(window, cx)
                }),
            );
        if self.entries.is_empty() {
            return root
                .child(
                    div()
                        .id(ids::RECENT_EMPTY)
                        .pt(dp(dims::RECENT_EMPTY_PAD_TOP))
                        .text_body1()
                        .text_color(tokens.on_background_secondary)
                        .child("Nothing to see here, open a repository first!")
                        .test_support(),
                )
                .test_support();
        }
        let this = cx.entity().downgrade();
        let clear = (!self.filter.is_empty()).then(|| {
            div()
                .absolute()
                .top_0()
                .right(dp(dims::RECENT_CLEAR_INSET))
                .h_full()
                .flex()
                .items_center()
                .child(
                    div()
                        .id(ids::RECENT_FILTER_CLEAR)
                        .size(dp(dims::RECENT_CLEAR))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(move |s| s.bg(tokens.hover_layer()))
                        .active(move |s| s.bg(tokens.press_layer()))
                        // Keep the focus in the filter field (a focusable host would take it).
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(move |_, window, cx| {
                            this.update(cx, |this, cx| {
                                this.clear_filter(window, cx);
                                this.focus_search(window, cx);
                            })
                            .ok();
                        })
                        .child(icon(AppIcon::Close, metrics::ICON_SM, tokens.on_background))
                        .test_support(),
                )
        });
        let list = uniform_list(
            ids::RECENT_LIST,
            self.shown.len(),
            cx.processor(|this, range: Range<usize>, window, cx| this.rows(range, window, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();
        root.child(
            div()
                .flex_none()
                .relative()
                .w_full()
                .child(self.field.clone())
                .children(clear),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .pt(dp(dims::RECENT_LIST_PAD_TOP))
                .child(
                    div()
                        .relative()
                        .size_full()
                        .child(list)
                        .child(list_scrollbar("recent-scrollbar", &self.scroll)),
                ),
        )
        .test_support()
    }
}
