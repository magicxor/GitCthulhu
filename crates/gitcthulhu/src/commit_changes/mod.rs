//! The commit changes view (owned by G6 after G0b; 08 §5, 09 §4.8): the right pane while a commit, branch, tag or
//! stash is selected. A "Files changed" list (list or tree mode, optional path filter) sits above the full commit
//! message and the author footer.
//!
//! Loading: the details of the selected commit are read in the background (`backend::details::commit_details`).
//! The previous commit's content stays visible while the next one loads (anti-flicker); a progress bar appears
//! only when loading takes longer than 150 ms. Only one file can be selected; selecting it opens its diff in the
//! centre pane (`RepoUi::click_file`). After a click on a row the list has the keyboard focus: Up / Down select the
//! previous / next file (directory rows of tree mode are skipped). The loaded files are also published in `RepoUi::commit_files`, which is
//! where `RepoUi::diff_target` takes renames and the untracked files of a stash from.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    App, AppContext as _, AsyncApp, Context, Entity, FocusHandle, IntoElement, KeyBinding,
    Modifiers, Render, ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle,
    WeakEntity, Window,
};

use crate::backend::{CommitDetails, GitError, GitTime, ObjectId, details};
use crate::notifications::ToastKind;
use crate::tab::ui::{CloseableView, FileList};
use crate::tab::{ReadSlot, RepoUiEvent, TabCtx};
use crate::widgets::field::{SearchEvent, SearchField};

pub mod platform;
mod render;
pub mod rows;

use rows::Rows;

/// Key context of the files column (Up / Down move the file selection).
pub const COMMIT_CHANGES_CONTEXT: &str = "CommitChanges";

gpui_kit::actions!(
    gitcthulhu_commit_changes,
    [
        /// Selects the previous file of the list (Up).
        SelectPreviousFile,
        /// Selects the next file of the list (Down).
        SelectNextFile,
    ]
);

/// Element ids of the commit changes view.
pub mod ids {
    /// The commit changes view (the whole right pane).
    pub const COMMIT_CHANGES: &str = "commit-changes";
    /// The files column (header, filter, progress and list on the `background` colour).
    pub const FILES: &str = "commit-changes-files";
    /// The "Files changed" header bar.
    pub const HEADER: &str = "commit-changes-header";
    /// The list/tree toggle in the header.
    pub const TOGGLE_TREE: &str = "commit-changes-toggle-tree";
    /// The search toggle in the header.
    pub const TOGGLE_SEARCH: &str = "commit-changes-toggle-search";
    /// The search field (inner field `commit-changes-search-field`, close button `commit-changes-search-close`).
    pub const SEARCH: &str = "commit-changes-search";
    /// The loading progress bar.
    pub const PROGRESS: &str = "commit-changes-progress";
    /// The scrolling list of file rows.
    pub const LIST: &str = "commit-changes-list";
    /// File row `(ROW, row index)`.
    pub const ROW: &str = "commit-changes-row";
    /// Directory row `(DIR_ROW, row index)` (tree mode).
    pub const DIR_ROW: &str = "commit-changes-dir-row";
    /// The commit message box (fixed height).
    pub const MESSAGE: &str = "commit-changes-message";
    /// The author footer.
    pub const FOOTER: &str = "commit-changes-footer";
    /// The author avatar.
    pub const AVATAR: &str = "commit-changes-avatar";
    /// The short hash (click copies the full hash).
    pub const HASH: &str = "commit-changes-hash";
    /// The "Copied!" confirmation.
    pub const COPIED: &str = "commit-changes-copied";
    /// The error line shown instead of the list when the details cannot be read.
    pub const ERROR: &str = "commit-changes-error";
}

/// Height of the commit message box (08 §5.1).
pub(crate) const MESSAGE_H: f32 = 120.;
/// Height of the author footer (08 §5.1).
pub(crate) const FOOTER_H: f32 = 72.;
/// Size of the author avatar (08 §5.1).
pub(crate) const AVATAR_SIZE: f32 = 40.;
/// Bottom padding of the files column (08 §5.1).
pub(crate) const LIST_PAD_BOTTOM: f32 = 4.;
/// How long a load may take before the progress bar shows (08 §5.2).
pub const PROGRESS_DELAY: Duration = Duration::from_millis(150);
/// How long "Copied!" stays after copying the hash (08 §5.1).
pub const COPIED_FOR: Duration = Duration::from_secs(2);

/// The commit a view shows: its id and whether it is a stash (stashes also list their untracked files).
type Key = (ObjectId, bool);

/// The loaded content, with the strings the footer shows prepared once per load.
struct Loaded {
    key: Key,
    files: std::sync::Arc<Vec<crate::backend::CommitFile>>,
    first_parent: Option<ObjectId>,
    message: SharedString,
    author_name: SharedString,
    author_email: SharedString,
    full_hash: SharedString,
    short_hash: SharedString,
    committer_time: GitTime,
}

impl Loaded {
    fn new(key: Key, details: &CommitDetails) -> Self {
        let person = &details.info.author.person;
        Self {
            key,
            files: details.files.clone(),
            first_parent: details.info.parents.first().copied(),
            message: SharedString::from(details.message.trim_end().to_owned()),
            author_name: SharedString::from(String::from_utf8_lossy(&person.name).into_owned()),
            author_email: SharedString::from(String::from_utf8_lossy(&person.email).into_owned()),
            full_hash: SharedString::from(key.0.to_string()),
            short_hash: SharedString::from(key.0.to_hex_with_len(7).to_string()),
            committer_time: details.info.committer.time,
        }
    }
}

/// The open search field.
struct SearchState {
    field: Entity<SearchField>,
    _subscription: Subscription,
}

/// The commit changes view of a repository tab.
pub struct CommitChangesView {
    ctx: TabCtx,
    /// The commit the selection asks for.
    wanted: Option<Key>,
    /// The newest loaded content (possibly of a previous commit while the wanted one loads).
    loaded: Option<Loaded>,
    loading: bool,
    show_progress: bool,
    error: Option<SharedString>,
    slot: ReadSlot,
    progress_task: Option<Task<()>>,
    search: Option<SearchState>,
    /// Collapsed directories of tree mode (per pane, not persisted).
    collapsed: HashSet<Vec<u8>>,
    rows: Rc<Rows>,
    rows_tree: bool,
    rows_dirty: bool,
    scroll: UniformListScrollHandle,
    /// Keyboard focus of the list (taken by a click on a row); a zero-size target, so it is not an ancestor of the
    /// filter field.
    focus: FocusHandle,
    copied: bool,
    copied_task: Option<Task<()>>,
    /// Tests: holds a finished load back (virtual time) so the loading states can be observed.
    #[cfg(test)]
    test_load_delay: Duration,
    _subscriptions: Vec<Subscription>,
}

impl CommitChangesView {
    /// The commit changes view of the tab of `ctx`.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe_in(
                &ctx.ui,
                window,
                |this, _, event: &RepoUiEvent, window, cx| this.on_ui_event(event, window, cx),
            ),
            cx.observe_global::<crate::settings::SettingsGlobal>(|_, cx| cx.notify()),
        ];
        let mut view = Self {
            ctx,
            wanted: None,
            loaded: None,
            loading: false,
            show_progress: false,
            error: None,
            slot: ReadSlot::default(),
            progress_task: None,
            search: None,
            collapsed: HashSet::new(),
            rows: Rc::new(Rows::default()),
            rows_tree: false,
            rows_dirty: false,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            copied: false,
            copied_task: None,
            #[cfg(test)]
            test_load_delay: Duration::ZERO,
            _subscriptions: subscriptions,
        };
        view.sync_selection(cx);
        view.sync_search(window, cx);
        view
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// Whether the details of the selected commit are being read.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Whether the loading progress bar is shown (a load took longer than [`PROGRESS_DELAY`]).
    pub fn progress_visible(&self) -> bool {
        self.show_progress
    }

    /// Whether "Copied!" is shown.
    pub fn copied_visible(&self) -> bool {
        self.copied
    }

    /// The commit whose content is on display (it can lag behind the selection while that loads).
    pub fn shown_commit(&self) -> Option<ObjectId> {
        self.loaded.as_ref().map(|l| l.key.0)
    }

    fn on_ui_event(&mut self, event: &RepoUiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RepoUiEvent::SelectionChanged => self.sync_selection(cx),
            RepoUiEvent::ViewStackChanged => self.sync_search(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Starts loading when the selected commit changed.
    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        let key = self.ctx.ui.read(cx).selection.commit();
        if key == self.wanted {
            return;
        }
        self.wanted = key;
        self.error = None;
        self.copied = false;
        self.copied_task = None;
        let Some(key) = key else {
            // Not a commit-based selection: the status pane takes the right side; release the content.
            self.slot.cancel();
            self.loading = false;
            self.show_progress = false;
            self.progress_task = None;
            self.loaded = None;
            self.rows = Rc::new(Rows::default());
            self.ctx.ui.update(cx, |ui, _| ui.commit_files = None);
            // The hidden filter must not stay on the view stack: Escape would close it invisibly instead of acting
            // on what the user sees.
            if self.search.is_some() {
                self.close_search(cx);
            }
            return;
        };
        if self.loaded.as_ref().is_some_and(|l| l.key == key) {
            self.slot.cancel();
            self.loading = false;
            self.show_progress = false;
            self.progress_task = None;
            return;
        }
        self.start_load(key, cx);
    }

    fn start_load(&mut self, key: Key, cx: &mut Context<Self>) {
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            // The inert test backend has no repository to read.
            self.loading = false;
            return;
        };
        self.loading = true;
        self.show_progress = false;
        self.progress_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(PROGRESS_DELAY).await;
                this.update(cx, |view, cx| {
                    if view.loading {
                        view.show_progress = true;
                        cx.notify();
                    }
                })
                .ok();
            },
        ));
        let (id, stash) = key;
        self.slot.start(
            cx,
            |view| &mut view.slot,
            move |_cancel| details::commit_details(&repo, id, stash),
            move |view, result, cx| view.on_loaded(key, result, cx),
        );
    }

    fn on_loaded(
        &mut self,
        key: Key,
        result: Result<CommitDetails, GitError>,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        if !self.test_load_delay.is_zero() {
            let delay = self.test_load_delay;
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(delay).await;
                this.update(cx, |view, cx| view.finish_load(key, result, cx))
                    .ok();
            })
            .detach();
            return;
        }
        self.finish_load(key, result, cx);
    }

    fn finish_load(
        &mut self,
        key: Key,
        result: Result<CommitDetails, GitError>,
        cx: &mut Context<Self>,
    ) {
        if self.wanted != Some(key) {
            return;
        }
        self.loading = false;
        self.show_progress = false;
        self.progress_task = None;
        match result {
            Ok(details) => self.apply_details(key, &details, cx),
            Err(err) => {
                self.loaded = None;
                self.rows = Rc::new(Rows::default());
                self.ctx.ui.update(cx, |ui, _| ui.commit_files = None);
                let text = format!("Could not read the commit's changes: {err}");
                self.error = Some(SharedString::from(text.clone()));
                crate::app_view::show_toast(cx, Some(self.ctx.id), ToastKind::Error, text, None);
            }
        }
        cx.notify();
    }

    fn apply_details(&mut self, key: Key, details: &CommitDetails, cx: &mut Context<Self>) {
        let new_commit = self.loaded.as_ref().map(|l| l.key) != Some(key);
        self.error = None;
        self.loaded = Some(Loaded::new(key, details));
        let files = details.files.clone();
        self.ctx
            .ui
            .update(cx, |ui, _| ui.commit_files = Some((key.0, files)));
        self.rows_dirty = true;
        self.ensure_rows(cx);
        if new_commit && !self.rows.is_empty() {
            self.scroll.scroll_to_item(0, gpui_kit::ScrollStrategy::Top);
        }
    }

    /// Rebuilds the cached rows when the files, the filter, the list/tree mode or the collapsed set changed.
    fn ensure_rows(&mut self, cx: &App) {
        // Runs every frame: read the flag in place instead of cloning the whole settings.
        let tree = cx
            .try_global::<crate::settings::SettingsGlobal>()
            .is_some_and(|g| g.0.changes_as_tree);
        if !self.rows_dirty && self.rows_tree == tree {
            return;
        }
        self.rows_dirty = false;
        self.rows_tree = tree;
        let filter = self
            .ctx
            .ui
            .read(cx)
            .commit_files_filter
            .clone()
            .unwrap_or_default();
        self.rows = Rc::new(match &self.loaded {
            Some(loaded) => rows::build(&loaded.files, &filter, tree, &self.collapsed),
            None => Rows::default(),
        });
    }

    /// Whether the list on screen belongs to the selected commit (clicks on a stale list are ignored).
    fn list_is_current(&self) -> bool {
        self.loaded.as_ref().map(|l| l.key) == self.wanted
    }

    // -- search ---------------------------------------------------------------------------------------------

    /// The header's magnifier: shows the filter field (focused) or hides it, which clears the filter (08 §4.3).
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_some() {
            self.close_search(cx);
            return;
        }
        self.ctx.ui.update(cx, |ui, cx| {
            ui.commit_files_filter = Some(SharedString::default());
            ui.push_view(CloseableView::CommitChangesSearch, cx);
        });
        self.create_search(window, cx);
        if let Some(search) = &self.search {
            search
                .field
                .clone()
                .update(cx, |field, cx| field.focus(window, cx));
        }
    }

    fn create_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field =
            cx.new(|cx| SearchField::new(ids::SEARCH, "Search files by name or path", window, cx));
        let subscription =
            cx.subscribe(&field, |this, field, event: &SearchEvent, cx| match event {
                SearchEvent::Changed => {
                    let query = field.read(cx).query(cx);
                    this.ctx
                        .ui
                        .update(cx, |ui, _| ui.commit_files_filter = Some(query));
                    this.rows_dirty = true;
                    cx.notify();
                }
                SearchEvent::Close => this.close_search(cx),
            });
        self.search = Some(SearchState {
            field,
            _subscription: subscription,
        });
    }

    /// Hides the filter field and clears the filter.
    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        self.rows_dirty = true;
        self.ctx.ui.update(cx, |ui, cx| {
            ui.views.remove(CloseableView::CommitChangesSearch);
            ui.commit_files_filter = None;
            cx.emit(RepoUiEvent::ViewStackChanged);
            cx.notify();
        });
        cx.notify();
    }

    /// Follows the view stack: Escape closes the filter through `RepoUi::close_last_view`.
    fn sync_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.ctx.ui.read(cx).commit_files_filter.is_some();
        match (wanted, self.search.is_some()) {
            (true, false) => self.create_search(window, cx),
            (false, true) => {
                self.search = None;
                self.rows_dirty = true;
            }
            _ => {}
        }
    }

    // -- list ------------------------------------------------------------------------------------------------

    fn toggle_tree_mode(&mut self, cx: &mut Context<Self>) {
        crate::settings::update(cx, |settings| {
            settings.changes_as_tree = !settings.changes_as_tree
        });
        cx.notify();
    }

    fn toggle_dir(&mut self, path: Vec<u8>, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
        self.rows_dirty = true;
        cx.notify();
    }

    /// Up / Down: selects the previous / next file row like a click (the diff follows) and scrolls it into view. A
    /// selection that is not in the list (filtered out, in a collapsed directory) moves to the first / last file.
    fn move_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.list_is_current() {
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        let selected = self
            .ctx
            .ui
            .read(cx)
            .files
            .single(FileList::CommitFiles)
            .cloned();
        let path_of = |row: usize| Some(&loaded.files.get(self.rows.file_index(row)?)?.path);
        let from = selected
            .and_then(|selected| (0..self.rows.len()).find(|&row| path_of(row) == Some(&selected)));
        let Some(row) = self.rows.adjacent_file_row(from, forward) else {
            return;
        };
        let Some(path) = path_of(row).cloned() else {
            return;
        };
        self.ctx.ui.update(cx, |ui, cx| {
            ui.click_file(
                FileList::CommitFiles,
                path.clone(),
                Modifiers::default(),
                std::slice::from_ref(&path),
                cx,
            )
        });
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    // -- footer ----------------------------------------------------------------------------------------------

    /// Copies the full hash and shows "Copied!" for two seconds (08 §5.1).
    fn copy_hash(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        platform::copy_text(cx, &loaded.full_hash);
        self.copied = true;
        self.copied_task = Some(cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(COPIED_FOR).await;
                this.update(cx, |view, cx| {
                    view.copied = false;
                    cx.notify();
                })
                .ok();
            },
        ));
        cx.notify();
    }

    /// Tests: holds finished loads back for `delay` of virtual time.
    #[cfg(test)]
    pub(crate) fn set_test_load_delay(&mut self, delay: Duration) {
        self.test_load_delay = delay;
    }
}

impl Render for CommitChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_view(window, cx)
    }
}

/// Registers the view's key bindings (G6): Up / Down in [`COMMIT_CHANGES_CONTEXT`]. Escape (closing the filter) is
/// handled by the repository frame through `RepoUi::close_last_view`, and the committed-file menu is built by
/// `menus::commit_files`.
pub fn register(cx: &mut App) {
    let context = Some(COMMIT_CHANGES_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPreviousFile, context),
        KeyBinding::new("down", SelectNextFile, context),
    ]);
}

#[cfg(test)]
mod tests;
