//! The staged/unstaged changes lists (owned by G5; 08 §4.2-§4.5, spec 21 §4.4): header with the list/tree toggle,
//! the file filter and the stage-all button; a virtualised list or tree of file rows with multi-selection through
//! `RepoUi::click_file`, the hover action, double-click staging and context menus.
//!
//! The rows are plain elements built for the visible range only (no per-row entities). The row data is rebuilt
//! when the status, the filter, the view mode or the collapsed set change, never per frame.

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    UniformListScrollHandle, Window, div, uniform_list,
};

use super::ListIds;
use super::actions;
use super::data::ListData;
use super::ids;
use crate::backend::{Status, StatusEntry};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuKind, MenuTarget};
use crate::metrics::{self, dp};
use crate::settings::SettingsGlobal;
use crate::tab::TabCtx;
use crate::tab::ui::{CloseableView, FileList, RepoUi, RepoUiEvent};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::SearchEvent;
use crate::widgets::file_row::{DirectoryRow, FileRow};
use crate::widgets::file_tree::TreeRow;
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{
    HeaderBar34, IconButton, SearchField, SecondaryButton, TooltipExt as _, observed,
};

/// The text of the header button (08 §4.2): "all" for 0-1 selected files of this list, "selected" for 2 or more.
pub fn header_button_label(list: FileList, selected: usize) -> &'static str {
    match (list, selected >= 2) {
        (FileList::Staged, false) => "Unstage all",
        (FileList::Staged, true) => "Unstage selected",
        (_, false) => "Stage all",
        (_, true) => "Stage selected",
    }
}

/// What the cached rows were built from. The status is held (not just its address) so a freed snapshot's
/// address cannot be reused by a newer one and leave stale rows behind.
#[derive(Clone)]
struct BuildKey {
    status: Option<Arc<Status>>,
    filter: Option<SharedString>,
    tree: bool,
    collapsed_rev: u64,
}

impl PartialEq for BuildKey {
    fn eq(&self, other: &Self) -> bool {
        let same_status = match (&self.status, &other.status) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_status
            && self.filter == other.filter
            && self.tree == other.tree
            && self.collapsed_rev == other.collapsed_rev
    }
}

/// One of the two changes lists of the status pane.
pub struct ChangesList {
    ctx: TabCtx,
    list: FileList,
    search: Entity<SearchField>,
    scroll: UniformListScrollHandle,
    collapsed: HashSet<Vec<u8>>,
    collapsed_rev: u64,
    data: Arc<ListData>,
    built: Option<BuildKey>,
    _subscriptions: Vec<Subscription>,
}

impl ChangesList {
    /// The staged (`FileList::Staged`) or unstaged list of the tab of `ctx`.
    pub fn new(ctx: TabCtx, list: FileList, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ids = ids::of(list);
        let search =
            cx.new(|cx| SearchField::new(ids.search, "Search files by name or path", window, cx));
        let subscriptions = vec![
            cx.subscribe(
                &search,
                |this, search, event: &SearchEvent, cx| match event {
                    SearchEvent::Changed => {
                        let query = search.read(cx).query(cx);
                        this.set_filter_text(query, cx);
                    }
                    SearchEvent::Close => this.close_search(cx),
                },
            ),
            cx.observe(&ctx.model, |_, _, cx| cx.notify()),
            cx.observe(&ctx.ui, |_, _, cx| cx.notify()),
            cx.observe_global::<SettingsGlobal>(|_, cx| cx.notify()),
        ];
        Self {
            ctx,
            list,
            search,
            scroll: UniformListScrollHandle::new(),
            collapsed: HashSet::new(),
            collapsed_rev: 0,
            data: Arc::new(ListData::build(&[], None, false, &HashSet::new())),
            built: None,
            _subscriptions: subscriptions,
        }
    }

    /// The search field entity (tests focus and type into it).
    pub fn search(&self) -> &Entity<SearchField> {
        &self.search
    }

    /// The displayed rows (tests).
    pub fn data(&self) -> &Arc<ListData> {
        &self.data
    }

    fn ids(&self) -> ListIds {
        ids::of(self.list)
    }

    fn view(&self) -> CloseableView {
        match self.list {
            FileList::Staged => CloseableView::StagedSearch,
            _ => CloseableView::UnstagedSearch,
        }
    }

    fn filter<'a>(&self, ui: &'a RepoUi) -> Option<&'a SharedString> {
        match self.list {
            FileList::Staged => ui.staged_filter.as_ref(),
            _ => ui.unstaged_filter.as_ref(),
        }
    }

    fn filter_mut<'a>(&self, ui: &'a mut RepoUi) -> &'a mut Option<SharedString> {
        match self.list {
            FileList::Staged => &mut ui.staged_filter,
            _ => &mut ui.unstaged_filter,
        }
    }

    fn entries_of_status<'a>(&self, status: &'a Status) -> &'a [StatusEntry] {
        match self.list {
            FileList::Staged => &status.staged,
            _ => &status.unstaged,
        }
    }

    /// Rebuilds the rows when something they depend on changed.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let status = self.ctx.model.read(cx).status.value.clone();
        let filter = self.filter(self.ctx.ui.read(cx)).cloned();
        let tree = super::view_settings(cx).0;
        let key = BuildKey {
            status,
            filter,
            tree,
            collapsed_rev: self.collapsed_rev,
        };
        if self.built.as_ref() == Some(&key) {
            return;
        }
        let empty: [StatusEntry; 0] = [];
        let entries = key
            .status
            .as_deref()
            .map_or(&empty[..], |s| self.entries_of_status(s));
        self.data = Arc::new(ListData::build(
            entries,
            key.filter.as_deref(),
            tree,
            &self.collapsed,
        ));
        self.built = Some(key);
    }

    // ── search ────────────────────────────────────────────────────────────────────────────────

    fn set_filter_text(&mut self, query: SharedString, cx: &mut Context<Self>) {
        self.ctx.ui.update(cx, |ui, cx| {
            let slot = self.filter_mut(ui);
            if slot.is_some() && slot.as_ref() != Some(&query) {
                *slot = Some(query);
                cx.notify();
            }
        });
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search.read(cx).query(cx);
        let view = self.view();
        self.ctx.ui.update(cx, |ui, cx| {
            *self.filter_mut(ui) = Some(query);
            ui.push_view(view, cx);
        });
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        let view = self.view();
        self.ctx.ui.update(cx, |ui, cx| {
            *self.filter_mut(ui) = None;
            ui.views.remove(view);
            cx.emit(RepoUiEvent::ViewStackChanged);
            cx.notify();
        });
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter(self.ctx.ui.read(cx)).is_some() {
            self.close_search(cx);
        } else {
            self.open_search(window, cx);
        }
    }

    // ── selection and actions ────────────────────────────────────────────────────────────────

    /// The selected entries of this list (empty when the selection lives in the other list).
    fn selected_entries(&self, cx: &App) -> Vec<StatusEntry> {
        let ui = self.ctx.ui.read(cx);
        if ui.files.list != Some(self.list) {
            return Vec::new();
        }
        self.data
            .entries_of(&ui.files.paths)
            .into_iter()
            .cloned()
            .collect()
    }

    fn raw_unstaged(&self, cx: &App) -> Vec<StatusEntry> {
        self.ctx
            .model
            .read(cx)
            .status
            .value
            .as_ref()
            .map(|s| s.unstaged.clone())
            .unwrap_or_default()
    }

    fn stage_or_unstage(&self, entries: Vec<StatusEntry>, window: &mut Window, cx: &mut App) {
        match self.list {
            FileList::Staged => actions::unstage(&self.ctx, entries, window, cx),
            _ => actions::stage(&self.ctx, entries, window, cx),
        }
    }

    fn click_row(
        &mut self,
        ix: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.data.entry_of_row(ix).cloned() else {
            return;
        };
        if event.click_count() >= 2 {
            self.stage_or_unstage(vec![entry], window, cx);
            return;
        }
        let order = self.data.visible_paths();
        let (list, mods) = (self.list, event.modifiers());
        self.ctx.ui.update(cx, |ui, cx| {
            ui.click_file(list, entry.path.clone(), mods, &order, cx)
        });
    }

    fn row_action(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.data.entry_of_row(ix).cloned() {
            self.stage_or_unstage(vec![entry], window, cx);
        }
    }

    fn header_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_entries(cx);
        if selected.len() >= 2 {
            self.stage_or_unstage(selected, window, cx);
        } else if self.list == FileList::Staged {
            actions::unstage_all(&self.ctx, window, cx);
        } else {
            let unstaged = self.raw_unstaged(cx);
            actions::stage_all(&self.ctx, &unstaged, window, cx);
        }
    }

    fn toggle_dir(&mut self, path: Vec<u8>, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.collapsed_rev += 1;
        cx.notify();
    }

    fn menu_target(&self, path: crate::backend::RelaPath) -> MenuTarget {
        MenuTarget {
            kind: MenuKind::StatusFile,
            tab: self.ctx.clone(),
            branch: None,
            upstream: None,
            commit: None,
            path: Some(path),
            is_current: false,
            checked_out_elsewhere: false,
        }
    }

    fn row_menu(
        &mut self,
        ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.data.entry_of_row(ix).cloned() else {
            return;
        };
        let target = self.menu_target(entry.path.clone());
        let multi = {
            let ui = self.ctx.ui.read(cx);
            (ui.files.list == Some(self.list)
                && ui.files.paths.len() >= 2
                && ui.files.paths.contains(&entry.path))
            .then(|| self.selected_entries(cx))
        };
        let items = match multi {
            Some(entries) => menus::files::multi_file_menu(&target, self.list, entries, cx),
            None => menus::files::single_file_menu(&target, self.list, &entry, cx),
        };
        menus::open_context_menu(window, cx, position, items);
    }

    fn dir_menu(
        &mut self,
        ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(TreeRow::Dir { path, .. }) = self.data.rows.get(ix) else {
            return;
        };
        let dir = crate::backend::RelaPath::from(path.as_slice());
        let below: Vec<StatusEntry> = self.data.entries_under(path).into_iter().cloned().collect();
        let target = self.menu_target(dir.clone());
        let items = menus::files::directory_menu(&target, self.list, dir, below, cx);
        menus::open_context_menu(window, cx, position, items);
    }

    // ── rendering ────────────────────────────────────────────────────────────────────────────

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let ids = self.ids();
        let this = cx.entity().downgrade();
        let ui = self.ctx.ui.read(cx);
        let selected = if ui.files.list == Some(self.list) {
            ui.files.paths.len()
        } else {
            0
        };
        let unstage_blocked = self.list == FileList::Staged
            && selected < 2
            && self
                .ctx
                .model
                .read(cx)
                .status
                .value
                .as_ref()
                .is_some_and(|s| s.counts.unstaged.conflicted > 0);
        let tree = self.data.tree;
        let (title, action_role, action_bg, action_fg) = match self.list {
            FileList::Staged => ("Staged", AppIcon::RemoveDone, tokens.error, tokens.on_error),
            _ => ("Unstaged", AppIcon::Done, tokens.primary, tokens.on_primary),
        };
        let label = header_button_label(self.list, selected);
        let search_this = this.clone();
        let action_this = this;
        HeaderBar34::new()
            .id(ids.header)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(dp(metrics::status::HEADER_TITLE_PAD_START))
                    .pr(dp(metrics::status::HEADER_TITLE_PAD_END))
                    .text_body2()
                    .text_color(tokens.on_background)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                IconButton::new(
                    ids.tree_toggle,
                    if tree { AppIcon::List } else { AppIcon::Tree },
                )
                .on_click(move |_, cx| {
                    crate::settings::update(cx, |s| s.changes_as_tree = !s.changes_as_tree);
                }),
            )
            .child(
                IconButton::new(ids.search_toggle, AppIcon::Search).on_click(move |window, cx| {
                    search_this
                        .update(cx, |list, cx| list.toggle_search(window, cx))
                        .ok();
                }),
            )
            .child(header_action_button(
                ids.action,
                action_role,
                label,
                action_bg,
                action_fg,
                !unstage_blocked,
                unstage_blocked.then_some("Resolve the conflicts first"),
                move |window, cx| {
                    action_this
                        .update(cx, |list, cx| list.header_action(window, cx))
                        .ok();
                },
            ))
            .into_any_element()
    }

    fn rows(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let ids = self.ids();
        let this = cx.entity().downgrade();
        let ctx = self.ctx.clone();
        let data = self.data.clone();
        let list = self.list;
        let rows = uniform_list(ids.rows, data.rows.len(), move |range, _, cx| {
            let ui = ctx.ui.read(cx);
            let same_list = ui.files.list == Some(list);
            range
                .filter_map(|ix| {
                    let row = data.rows.get(ix)?;
                    Some(match row {
                        TreeRow::Dir {
                            path,
                            name,
                            depth,
                            expanded,
                        } => {
                            let (toggle_this, menu_this) = (this.clone(), this.clone());
                            let toggle_path = path.clone();
                            div()
                                .id(ElementId::from((ids.dir, ix)))
                                .w_full()
                                .flex_none()
                                .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
                                    let position = ev.position;
                                    menu_this
                                        .update(cx, |l, cx| l.dir_menu(ix, position, window, cx))
                                        .ok();
                                })
                                .child(
                                    DirectoryRow::new(
                                        (ids.dir_row, ix),
                                        String::from_utf8_lossy(name).into_owned(),
                                        *depth,
                                        *expanded,
                                    )
                                    .on_toggle(move |_, cx| {
                                        toggle_this
                                            .update(cx, |l, cx| {
                                                l.toggle_dir(toggle_path.clone(), cx)
                                            })
                                            .ok();
                                    }),
                                )
                                .into_any_element()
                        }
                        TreeRow::File { index, depth, .. } => {
                            let entry = data.entries.get(*index)?;
                            let selected = same_list && ui.files.paths.contains(&entry.path);
                            let (click_this, menu_this, action_this) =
                                (this.clone(), this.clone(), this.clone());
                            let (label, background) = match list {
                                FileList::Staged => ("Unstage", tokens.error),
                                _ => ("Stage", tokens.primary),
                            };
                            let mut row =
                                FileRow::new((ids.row, ix), entry.path.to_vec(), entry.kind)
                                    .selected(selected)
                                    .trailing(
                                        SecondaryButton::new((ids.row_action, ix), label)
                                            .background(background)
                                            .on_click(move |window, cx| {
                                                // The button sits inside the row: the row must not also select.
                                                cx.stop_propagation();
                                                action_this
                                                    .update(cx, |l, cx| {
                                                        l.row_action(ix, window, cx)
                                                    })
                                                    .ok();
                                            }),
                                    )
                                    .on_click(move |event, window, cx| {
                                        click_this
                                            .update(cx, |l, cx| l.click_row(ix, event, window, cx))
                                            .ok();
                                    })
                                    .on_context_menu(move |event, window, cx| {
                                        let position = event.position;
                                        menu_this
                                            .update(cx, |l, cx| {
                                                l.row_menu(ix, position, window, cx)
                                            })
                                            .ok();
                                    });
                            if data.tree {
                                row = row.tree(*depth);
                            }
                            row.into_any_element()
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .size_full();
        observed(ids.list_area)
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(tokens.background)
            .child(rows)
            .child(list_scrollbar(ids.scrollbar, &self.scroll))
            .into_any_element()
    }
}

/// The coloured header button of a status list (08 §4.2 `SecondaryButtonCompactable`, always expanded): icon,
/// label, action colour; 50 % alpha without click while disabled.
#[allow(clippy::too_many_arguments)]
fn header_action_button(
    id: &'static str,
    role: AppIcon,
    label: &'static str,
    background: gpui_kit::Hsla,
    foreground: gpui_kit::Hsla,
    enabled: bool,
    tooltip: Option<&'static str>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    use crate::metrics::status as m;
    let alpha = if enabled { 1.0 } else { 0.5 };
    div()
        .flex_none()
        .ml(dp(m::HEADER_ACTION_MARGIN_START))
        .mr(dp(m::HEADER_ACTION_MARGIN_END))
        .child(
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .px(dp(m::HEADER_ACTION_PAD_H))
                .rounded(dp(metrics::RADIUS))
                .bg(background.opacity(alpha))
                .text_color(foreground.opacity(alpha))
                .cursor_pointer()
                .when(enabled, |b| {
                    b.on_click(move |_, window, cx| on_click(window, cx))
                })
                .when_some(tooltip, |b, text| b.instant_tooltip(text, None))
                .child(
                    div()
                        .flex_none()
                        .pl(dp(m::HEADER_ACTION_ICON_PAD_START))
                        .pr(dp(m::HEADER_ACTION_ICON_PAD_END))
                        .child(icon(role, metrics::ICON_MD, foreground.opacity(alpha))),
                )
                .child(
                    div()
                        .flex_none()
                        .py(dp(m::HEADER_ACTION_LABEL_PAD_V))
                        .pr(dp(m::HEADER_ACTION_LABEL_PAD_END))
                        .text_body2()
                        .whitespace_nowrap()
                        .child(label),
                )
                .test_support(),
        )
}

impl Render for ChangesList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let ids = self.ids();
        let filter_open = self.filter(self.ctx.ui.read(cx)).is_some();
        let header = self.header(cx);
        let rows = self.rows(cx);
        observed(ids.list)
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .when(filter_open, |list| list.child(self.search.clone()))
            .child(rows)
    }
}
