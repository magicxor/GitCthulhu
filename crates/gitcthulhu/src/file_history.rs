//! The file history view (owned by G8; 09 §2.2). The tab creates it while `RepoUi::history` is set; it replaces the
//! split area under the toolbar: header, a 300 dp list of the commits that changed the file (paged, followed
//! across renames) and the read-only diff of the clicked commit.
//!
//! Reads: the pager is created in the background together with its first page; the next page is requested when
//! the last visible row gets within [`m::PAGE_AHEAD`] rows of the end. The diff of a clicked commit is shown by a
//! read-only [`DiffView`] (the same list, selection, context menu, highlighting and line cutting as the main diff);
//! a click on another commit retargets it and cancels the read in flight.
//!
//! Like Gitnuro (09 §2.2) the view opens with no commit selected and an empty diff area; the diff appears on the
//! first click.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, UniformListScrollHandle, WeakEntity, Window, div, uniform_list,
};
use parking_lot::Mutex;

use crate::app_view::show_toast;
use crate::backend::history::HistoryPager;
use crate::backend::{
    Against, ByteSlice as _, CancelToken, DiffTarget, FileHistoryPage, GitError, GitErrorKind,
    GitTime, RelaPath,
};
use crate::blame::view_header;
use crate::diff::DiffView;
use crate::diff::model::DiffDoc;
use crate::metrics::dp;
use crate::metrics::history as m;
use crate::notifications::ToastKind;
use crate::settings::{self, SettingsGlobal};
use crate::tab::TabCtx;
use crate::tab::read::ReadSlot;
use crate::tab::ui::{CloseableView, RepoUiEvent};
use crate::theme::ActiveTokens as _;
use crate::widgets::date::{DateFormat, local_offset_minutes};
use crate::widgets::observed;
use crate::widgets::scroll::list_scrollbar;

mod rows;

pub use rows::HistoryItem;

#[cfg(test)]
mod tests;

/// Element ids of the file history view.
pub mod ids {
    /// The file history view.
    pub const FILE_HISTORY: &str = "file-history";
    /// The header bar.
    pub const HISTORY_HEADER: &str = "history-header";
    /// The header's close button.
    pub const HISTORY_CLOSE: &str = "history-close";
    /// The path text of the header.
    pub const HISTORY_TITLE: &str = "history-title";
    /// The commit list area (300 dp).
    pub const HISTORY_LIST: &str = "history-list";
    /// Row `ix` of the commit list (tuple id).
    pub const HISTORY_ROW: &str = "history-row";
    /// The date text of row `ix` (tuple id).
    pub const HISTORY_DATE: &str = "history-date";
    /// The diff area.
    pub const HISTORY_DIFF: &str = "history-diff";
}

/// Opens the history of `path` (replaces everything under the toolbar; Esc closes it). The "File history" menu
/// entries call this.
pub fn open(ctx: &TabCtx, path: RelaPath, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        ui.history = Some(path);
        ui.push_view(CloseableView::FileHistory, cx);
        cx.emit(RepoUiEvent::CenterChanged);
    });
}

/// Closes the history (and drops its Esc entry).
pub fn close(ctx: &TabCtx, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        if ui.history.take().is_none() {
            return;
        }
        ui.views.remove(CloseableView::FileHistory);
        cx.emit(RepoUiEvent::ViewStackChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
    });
}

/// What a page read returns: the (possibly new) pager and the page.
type PageResult = Result<(Arc<Mutex<HistoryPager>>, FileHistoryPage), GitError>;

/// The history of one file.
pub struct FileHistoryView {
    ctx: TabCtx,
    path: RelaPath,
    items: Arc<Vec<HistoryItem>>,
    pager: Option<Arc<Mutex<HistoryPager>>>,
    exhausted: bool,
    failed: bool,
    page: ReadSlot,
    /// The clicked row (no highlight, 09 §2.2; kept for reloads).
    selected: Option<usize>,
    /// The diff of the selected commit: unified/split, full file and the scroll position follow the settings and
    /// the target by themselves.
    diff_view: Entity<DiffView>,
    /// The date format re-formats the rows when it changes.
    date_format: DateFormat,
    list_scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl FileHistoryView {
    /// Whether a page of the history or the selected commit's diff is loading (the QA runner's idle check).
    pub fn is_loading(&self, cx: &App) -> bool {
        self.page.is_running() || self.diff_view.read(cx).is_loading()
    }

    /// The history of `path` in the tab of `ctx`.
    pub fn new(ctx: TabCtx, path: RelaPath, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings::get_or_default(cx);
        let subscriptions = vec![cx.observe_global::<SettingsGlobal>(|this, cx| {
            this.on_settings(cx);
        })];
        let diff_view = cx.new(|cx| DiffView::read_only(ctx.clone(), window, cx));
        let mut view = Self {
            ctx,
            path,
            items: Arc::new(Vec::new()),
            pager: None,
            exhausted: false,
            failed: false,
            page: ReadSlot::default(),
            selected: None,
            diff_view,
            date_format: DateFormat::from_settings(&settings),
            list_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        view.load_page(cx);
        view
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The file whose history is shown.
    pub fn path(&self) -> &RelaPath {
        &self.path
    }

    /// The commits loaded so far, newest first.
    pub fn items(&self) -> &[HistoryItem] {
        &self.items
    }

    /// Whether the walk has no more commits.
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// The document of the diff on the right, if one is shown.
    pub fn diff_doc(&self, cx: &App) -> Option<Arc<DiffDoc>> {
        self.diff_view.read(cx).doc().cloned()
    }

    /// The read-only diff view on the right.
    pub fn diff_view(&self) -> &Entity<DiffView> {
        &self.diff_view
    }

    fn now() -> GitTime {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        GitTime {
            seconds,
            offset_minutes: local_offset_minutes(seconds),
        }
    }

    /// Requests the next page unless one is running or the walk is over.
    fn load_page(&mut self, cx: &mut Context<Self>) {
        if self.exhausted || self.failed || self.page.is_running() {
            return;
        }
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        let pager = self.pager.clone();
        let path = self.path.clone();
        self.page.start(
            cx,
            |this| &mut this.page,
            move |cancel: &CancelToken| -> PageResult {
                let pager = match pager {
                    Some(pager) => pager,
                    None => Arc::new(Mutex::new(HistoryPager::new(&repo, &path)?)),
                };
                let page = pager.lock().next(m::PAGE_SIZE, cancel)?;
                Ok((pager, page))
            },
            |this, result, cx| this.apply_page(result, cx),
        );
    }

    fn apply_page(&mut self, result: PageResult, cx: &mut Context<Self>) {
        match result {
            Ok((pager, page)) => {
                self.pager = Some(pager);
                self.exhausted = page.exhausted;
                let now = Self::now();
                let format = self.date_format.clone();
                let items = Arc::make_mut(&mut self.items);
                items.extend(
                    page.entries
                        .into_iter()
                        .map(|e| HistoryItem::new(e, now, &format)),
                );
            }
            Err(err) if err.kind() == GitErrorKind::Cancelled => {}
            Err(err) => {
                self.failed = true;
                show_toast(
                    cx,
                    Some(self.ctx.id),
                    ToastKind::Error,
                    format!("Loading the file history failed: {err}"),
                    None,
                );
            }
        }
        cx.notify();
    }

    /// A click on row `ix`: shows the diff of that commit.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(ix) else {
            return;
        };
        let target = DiffTarget::Commit {
            commit: item.entry.commit.id,
            against: Against::FirstParent,
            path: item.entry.path.clone(),
            previous_path: item.entry.previous_path.clone(),
        };
        self.selected = Some(ix);
        self.diff_view
            .update(cx, |view, cx| view.set_target(Some(target), cx));
        cx.notify();
    }

    fn on_settings(&mut self, cx: &mut Context<Self>) {
        let settings = settings::get_or_default(cx);
        let format = DateFormat::from_settings(&settings);
        if format != self.date_format {
            self.date_format = format;
            let (now, format) = (Self::now(), self.date_format.clone());
            self.items = Arc::new(
                self.items
                    .iter()
                    .cloned()
                    .map(|item| item.with_date(now, &format))
                    .collect(),
            );
        }
        cx.notify();
    }

    fn list(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let tokens = *cx.tokens();
        let view: WeakEntity<Self> = cx.weak_entity();
        let on_select = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| this.select(ix, cx)).ok();
            }) as Rc<dyn Fn(usize, &mut Window, &mut App)>
        };
        let env = Rc::new(rows::RowEnv { tokens, on_select });
        let items = self.items.clone();
        let count = items.len();
        let more = !self.exhausted && !self.failed;
        let list = uniform_list(ids::HISTORY_LIST, count, move |range, window, cx| {
            if more && range.end + m::PAGE_AHEAD >= count {
                let view = view.clone();
                window.defer(cx, move |_, cx| {
                    view.update(cx, |this, cx| this.load_page(cx)).ok();
                });
            }
            range
                .filter_map(|ix| items.get(ix).map(|item| rows::row(&env, item, ix)))
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.list_scroll)
        .size_full();
        observed(ids::HISTORY_LIST)
            .relative()
            .flex_none()
            .w(dp(m::LIST_W))
            .h_full()
            .bg(tokens.surface)
            .child(list)
            .child(list_scrollbar("history-list-scrollbar", &self.list_scroll))
            .into_any_element()
    }

    fn diff_area(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let tokens = *cx.tokens();
        // Nothing is shown before the first click; the read-only view renders nothing without a target either.
        observed(ids::HISTORY_DIFF)
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(tokens.background)
            .child(self.diff_view.clone())
            .into_any_element()
    }
}

impl Render for FileHistoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let ctx = self.ctx.clone();
        let path = SharedString::from(self.path.to_str_lossy().into_owned());
        observed(ids::FILE_HISTORY)
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .child(view_header(
                ids::HISTORY_HEADER,
                ids::HISTORY_CLOSE,
                ids::HISTORY_TITLE,
                path,
                move |_, cx| close(&ctx, cx),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .child(self.list(cx))
                    .child(self.diff_area(cx)),
            )
    }
}

/// Registers the file history view's key bindings. Esc is the frame's `CloseLastView` (the history is a closeable
/// view), so there is nothing to bind.
pub fn register(_cx: &mut App) {}
