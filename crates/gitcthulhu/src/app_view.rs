//! `AppView`: the window content (spec 21 §2.1, §2.2): the tab slots and their API, the active tab's content, the
//! toast and menu layers, the request pump (credential prompts and confirmations from background threads) and the
//! error-dialog queue (09 §3.17: one at a time, oldest first).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, Styled as _, Task, WeakEntity, Window, div,
};

use crate::actions::{
    APP_CONTEXT, CloseTab, NewTab, NextTab, OpenRepository, OpenSettings, PrevTab,
};
use crate::backend::handle::{self, RepoHandle, TabBackend};
use crate::backend::present::{self, ErrorDialogData};
use crate::backend::prompt::{ConfirmRequest, UiRequest};
use crate::backend::{CancelToken, CloneOptions, GitError, GitErrorKind, GitStatus};
use crate::ids;
use crate::menus::MenuHost;
use crate::notifications::{ToastAction, ToastHost, ToastKind};
use crate::settings::state;
use crate::shell::tab_strip::TabStripView;
use crate::shell::welcome::WelcomeView;
use crate::tab::ops::Cancellable;
use crate::tab::{RepoTab, TabId, TabOptions};
use crate::theme::ActiveTokens as _;
use crate::widgets::observed;

/// The window's toast host.
pub struct ToastHostGlobal(pub Entity<ToastHost>);

impl Global for ToastHostGlobal {}

/// The window's AppView (one main window).
pub struct AppViewGlobal(pub WeakEntity<AppView>);

impl Global for AppViewGlobal {}

/// Shows a toast (deferred to the end of the current update, so it is safe from any context). `tab = None` shows it
/// on every tab. Dropped (logged) when no window exists.
pub fn show_toast(
    cx: &mut App,
    tab: Option<TabId>,
    kind: ToastKind,
    text: impl Into<SharedString>,
    action: Option<ToastAction>,
) {
    let text = text.into();
    cx.defer(move |cx| {
        let Some(host) = cx.try_global::<ToastHostGlobal>().map(|g| g.0.clone()) else {
            tracing::info!("toast without a window: {text}");
            return;
        };
        host.update(cx, |host, cx| {
            host.show(tab, kind, text, action, cx);
        });
    });
}

/// The Info toast of a feature whose owner has not implemented it yet ("Not implemented yet: {what}").
pub fn not_implemented(cx: &mut App, tab: Option<TabId>, what: &str) {
    show_toast(
        cx,
        tab,
        ToastKind::Positive,
        format!("Not implemented yet: {what}"),
        None,
    );
}

/// Queues an error dialog (shown one at a time, oldest first). Deferred, so it is safe from any context.
pub fn show_error(cx: &mut App, data: ErrorDialogData) {
    cx.defer(move |cx| {
        let Some(view) = cx.try_global::<AppViewGlobal>().and_then(|g| g.0.upgrade()) else {
            tracing::warn!("error without a window: {}: {}", data.title, data.message);
            return;
        };
        view.update(cx, |view, cx| {
            view.error_queue.push_back(data);
            cx.notify();
        });
    });
}

/// What a tab slot shows.
pub enum TabContent {
    /// The welcome page ("New tab").
    Welcome(Entity<WelcomeView>),
    /// A repository is being opened.
    Opening { path: PathBuf, task: Task<()> },
    /// An open repository.
    Repo(Entity<RepoTab>),
}

/// One tab of the strip.
pub struct TabSlot {
    pub id: TabId,
    pub content: TabContent,
}

impl TabSlot {
    /// The repository tab, if this slot shows one.
    pub fn repo(&self) -> Option<&Entity<RepoTab>> {
        match &self.content {
            TabContent::Repo(tab) => Some(tab),
            _ => None,
        }
    }

    /// The repository directory of a repository or opening slot.
    pub fn path(&self, cx: &App) -> Option<PathBuf> {
        match &self.content {
            TabContent::Welcome(_) => None,
            TabContent::Opening { path, .. } => Some(path.clone()),
            TabContent::Repo(tab) => Some(tab.read(cx).path().clone()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ErrorSlot {
    Idle,
    Opening,
    Showing,
}

/// The window content.
pub struct AppView {
    tabs: Vec<TabSlot>,
    active: usize,
    toasts: Entity<ToastHost>,
    menus: Entity<MenuHost>,
    tab_strip: Entity<TabStripView>,
    error_queue: VecDeque<ErrorDialogData>,
    error_slot: ErrorSlot,
    /// Slots a clone added next to a repository tab: removed again when the clone fails.
    clone_slots: Vec<TabId>,
    /// Slots still opening a tab restored from the last session.
    restoring: Vec<TabId>,
    /// Restored tabs whose repository could not be opened (an unplugged drive): kept in `state.json` behind the
    /// open tabs until the user opens another repository or closes the last tab, with the reasons for the summary.
    unrestored: Vec<(PathBuf, String)>,
    /// The user agreed to close the window although an operation runs (`should_close_window`).
    close_confirmed: bool,
    focus: FocusHandle,
    _request_pump: Task<()>,
    _bounds_save: Option<Task<()>>,
    _subscriptions: Vec<gpui_kit::Subscription>,
    /// Titles of the error dialogs opened so far (tests).
    #[cfg(test)]
    pub(crate) errors_opened: Vec<SharedString>,
    /// Message, details and hook output of the error dialogs opened so far (tests).
    #[cfg(test)]
    pub(crate) error_texts: Vec<String>,
}

impl AppView {
    /// The window content: restores the tabs of `UiState` and opens `cli` (a repository directory) in a new
    /// selected tab.
    pub fn new(
        requests: UnboundedReceiver<UiRequest>,
        cli: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let toasts = cx.new(|_| ToastHost::new());
        cx.set_global(ToastHostGlobal(toasts.clone()));
        let menus = MenuHost::install(cx);
        let weak = cx.weak_entity();
        cx.set_global(AppViewGlobal(weak.clone()));
        let tab_strip = cx.new(|cx| TabStripView::new(weak.clone(), window, cx));
        let request_pump = cx.spawn_in(window, async move |this, cx| {
            let mut requests = requests;
            while let Some(request) = requests.next().await {
                if this
                    .update_in(cx, |view, window, cx| {
                        view.handle_request(request, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let subscriptions = vec![cx.observe_window_bounds(window, |view, window, cx| {
            view.schedule_bounds_save(window, cx)
        })];
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |view, cx| view.should_close_window(window, cx))
                .unwrap_or(true)
        });
        let mut view = Self {
            tabs: Vec::new(),
            active: 0,
            toasts,
            menus,
            tab_strip,
            error_queue: VecDeque::new(),
            error_slot: ErrorSlot::Idle,
            clone_slots: Vec::new(),
            restoring: Vec::new(),
            unrestored: Vec::new(),
            close_confirmed: false,
            focus: cx.focus_handle(),
            _request_pump: request_pump,
            _bounds_save: None,
            _subscriptions: subscriptions,
            #[cfg(test)]
            errors_opened: Vec::new(),
            #[cfg(test)]
            error_texts: Vec::new(),
        };
        let restore = cx
            .try_global::<state::UiStateGlobal>()
            .map(|s| (s.0.tabs_open.clone(), s.0.selected_tab_index()))
            .unwrap_or_default();
        for path in restore.0 {
            let path = crate::app::strip_git_dir(path);
            let ix = view.target_slot(false, window, cx);
            view.restoring.push(view.tabs[ix].id);
            view.start_open(ix, path, OpenKind::Open, None, window, cx);
        }
        // A repository given on the command line that is already among the restored tabs selects that tab.
        let restored_cli = cli.as_ref().and_then(|path| {
            view.tabs.iter().position(|slot| {
                slot.path(cx)
                    .is_some_and(|open| crate::worktrees::same_path(&open, path))
            })
        });
        if let Some(ix) = restored_cli {
            view.active = ix;
        } else if let Some(path) = cli {
            view.open_repository(path, true, window, cx);
        } else if !view.tabs.is_empty() {
            view.active = restore.1.min(view.tabs.len() - 1);
        }
        if view.tabs.is_empty() {
            view.push_welcome(window, cx);
        }
        view.after_tabs_changed(window, cx);
        view
    }

    /// The tab slots, in strip order.
    pub fn tabs(&self) -> &[TabSlot] {
        &self.tabs
    }

    /// The index of the selected tab.
    pub fn active(&self) -> usize {
        self.active
    }

    /// The selected repository tab, if the selected slot shows one.
    pub fn active_tab(&self) -> Option<&Entity<RepoTab>> {
        self.tabs.get(self.active).and_then(TabSlot::repo)
    }

    /// The toast host of the window.
    pub fn toasts(&self) -> &Entity<ToastHost> {
        &self.toasts
    }

    /// The menu host of the window.
    pub fn menus(&self) -> &Entity<MenuHost> {
        &self.menus
    }

    /// Error dialogs waiting to be shown.
    pub fn pending_errors(&self) -> usize {
        self.error_queue.len()
    }

    /// Whether an error dialog of the queue is open (or opening) now.
    pub fn error_dialog_open(&self) -> bool {
        self.error_slot != ErrorSlot::Idle
    }

    /// The title of a tab (07 §2.2): the repository's last path segment, `.git` removed, or "New tab".
    pub fn tab_title(&self, ix: usize, cx: &App) -> SharedString {
        match self.tabs.get(ix).map(|s| &s.content) {
            Some(TabContent::Repo(tab)) => tab.read(cx).name(),
            Some(TabContent::Opening { path, .. }) => handle::display_name_of(path).into(),
            _ => "New tab".into(),
        }
    }

    /// The window title (07 §1): "GitCthulhu - " and the selected tab's title ("New tab" for a welcome tab),
    /// truncated to 250 characters.
    pub fn window_title(&self, cx: &App) -> String {
        format!("GitCthulhu - {}", self.tab_title(self.active, cx))
            .chars()
            .take(250)
            .collect()
    }

    /// The tooltip of a tab: the repository's full path (none for "New tab").
    pub fn tab_tooltip(&self, ix: usize, cx: &App) -> Option<SharedString> {
        self.tabs
            .get(ix)
            .and_then(|s| s.path(cx))
            .map(|p| SharedString::from(p.display().to_string()))
    }

    fn push_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let weak = cx.weak_entity();
        let welcome = cx.new(|cx| WelcomeView::new(weak, window, cx));
        self.tabs.push(TabSlot {
            id: TabId::next(),
            content: TabContent::Welcome(welcome),
        });
        self.tabs.len() - 1
    }

    fn welcome_content(&self, window: &mut Window, cx: &mut Context<Self>) -> TabContent {
        let weak = cx.weak_entity();
        TabContent::Welcome(cx.new(|cx| WelcomeView::new(weak, window, cx)))
    }

    /// A new empty ("New tab") tab, selected.
    pub fn new_empty_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = self.push_welcome(window, cx);
        self.after_tabs_changed(window, cx);
    }

    /// The slot a new repository goes to: the selected welcome tab when `select`, else a new slot.
    fn target_slot(&mut self, select: bool, window: &mut Window, cx: &mut Context<Self>) -> usize {
        if select
            && matches!(
                self.tabs.get(self.active).map(|s| &s.content),
                Some(TabContent::Welcome(_))
            )
        {
            return self.active;
        }
        let ix = self.push_welcome(window, cx);
        if select {
            self.active = ix;
        }
        ix
    }

    /// Opens the repository at (or above) `path` in a tab (07 §2.6, 05 §5.2). A failure shows "Could not open the
    /// repository" and the tab returns to the welcome page.
    pub fn open_repository(
        &mut self,
        path: PathBuf,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = crate::app::strip_git_dir(path);
        // The user moved on: the tabs of the last session that could not be reopened are forgotten.
        self.unrestored.clear();
        let ix = self.target_slot(select, window, cx);
        self.start_open(ix, path, OpenKind::Open, None, window, cx);
    }

    /// Opens `path` in the slot of the repository tab `id`, replacing that tab in place (07 §2.6: the toolbar's Open
    /// popup; `open_repository` would add a tab). Asks first when the tab's terminal reports a working shell
    /// (`terminal::confirm_close`), like closing the tab does. A no-op when the tab is gone.
    pub fn replace_repository(
        &mut self,
        id: TabId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.tabs.iter().position(|s| s.id == id) else {
            return;
        };
        let questions = self.close_questions(ix, cx);
        if questions.is_empty() {
            self.start_replace(id, path, window, cx);
            return;
        }
        let weak = cx.weak_entity();
        ask_all(
            questions,
            window,
            cx,
            Box::new(move |window, cx| {
                weak.update(cx, |view, cx| view.start_replace(id, path, window, cx))
                    .ok();
            }),
        );
    }

    /// The questions asked before the repository tab in slot `ix` goes away, in order: a running operation that
    /// may stop half-way (an operation that cannot be cancelled is left to finish, see `RepoTab::close`), then a
    /// program working in its terminal.
    fn close_questions(&self, ix: usize, cx: &App) -> Vec<ConfirmRequest> {
        let Some(tab) = self.tabs.get(ix).and_then(TabSlot::repo) else {
            return Vec::new();
        };
        let tab = tab.read(cx);
        let mut questions = Vec::new();
        if tab.busy_cancellable() == Some(Cancellable::WithWarning) {
            let mut request = ConfirmRequest::new(
                "Close the tab?",
                "The operation may stop half-way. Close anyway?",
            );
            request.confirm_label = "Close".into();
            request.cancel_label = "Keep running".into();
            request.destructive = true;
            questions.push(request);
        }
        questions.extend(crate::terminal::confirm_close(tab.ctx(), cx));
        questions
    }

    /// Asked when the window is about to close: `false` keeps it open and asks first when an operation that is not
    /// safe to kill is running (the process exit ends every Git it started). Closes the window itself once the
    /// user agrees.
    pub fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.close_confirmed {
            return true;
        }
        let running = self.tabs.iter().filter_map(TabSlot::repo).any(|tab| {
            tab.read(cx)
                .busy_cancellable()
                .is_some_and(|c| c != Cancellable::Yes)
        });
        if !running {
            return true;
        }
        let mut request = ConfirmRequest::new(
            "Quit GitCthulhu?",
            "An operation is still running. It may stop half-way. Quit anyway?",
        );
        request.confirm_label = "Quit".into();
        request.cancel_label = "Keep running".into();
        request.destructive = true;
        let weak = cx.weak_entity();
        crate::dialogs::confirm::open_with(
            window,
            cx,
            request,
            Box::new(move |yes, window, cx| {
                if yes {
                    weak.update(cx, |view, _| view.close_confirmed = true).ok();
                    window.remove_window();
                }
            }),
        );
        false
    }

    fn start_replace(
        &mut self,
        id: TabId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.tabs.iter().position(|s| s.id == id) else {
            return;
        };
        if let TabContent::Repo(tab) = &self.tabs[ix].content {
            tab.update(cx, |tab, cx| tab.close(cx));
        }
        // The slot keeps its id: the old repository's toasts must not show over the new one.
        self.toasts.update(cx, |t, cx| t.clear_tab(id, cx));
        let path = crate::app::strip_git_dir(path);
        self.start_open(ix, path, OpenKind::Open, None, window, cx);
    }

    /// `git init` at `path` and opens it in a new selected tab.
    pub fn init_repository(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.target_slot(true, window, cx);
        self.start_open(ix, path, OpenKind::Init, None, window, cx);
    }

    /// Clones in the background (no tab operation and no processing overlay: the clone dialog, 09 §3.1, shows the
    /// returned progress and cancels through the returned token) and opens the clone in a new selected tab.
    /// `on_done` runs on the UI thread when the clone ended: `Ok` after the tab opened, `Err` (also `Cancelled`) with
    /// the slot back on the welcome page. Clone failures are not queued as error dialogs: the clone dialog shows them
    /// in its error banner (09 §3.1).
    pub fn clone_repository(
        &mut self,
        opts: CloneOptions,
        on_done: OnCloneDone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CloneJob {
        let cancel = CancelToken::new();
        let (pump, progress) = crate::backend::prompt::ProgressPump::channel();
        let reuses_welcome = matches!(
            self.tabs.get(self.active).map(|s| &s.content),
            Some(TabContent::Welcome(_))
        );
        let ix = self.target_slot(true, window, cx);
        // A clone started from a repository tab got a new slot: a failed or cancelled clone removes it again
        // instead of leaving an extra "New tab" behind.
        if !reuses_welcome {
            self.clone_slots.push(self.tabs[ix].id);
        }
        let path = opts.directory.clone();
        self.start_open(
            ix,
            path,
            OpenKind::Clone(Box::new(opts), cancel.clone(), pump),
            Some(on_done),
            window,
            cx,
        );
        CloneJob { cancel, progress }
    }

    fn start_open(
        &mut self,
        ix: usize,
        path: PathBuf,
        kind: OpenKind,
        on_done: Option<OnCloneDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.tabs[ix].id;
        let open_path = path.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = open_in_background(open_path.clone(), kind, cx).await;
            this.update_in(cx, |view, window, cx| {
                view.finish_open(id, open_path, result, on_done, window, cx)
            })
            .ok();
        });
        self.tabs[ix].content = TabContent::Opening { path, task };
        // The welcome page that had the focus is gone: keep the keyboard on the window (tab shortcuts) meanwhile.
        if ix == self.active {
            window.focus(&self.focus, cx);
        }
        self.after_tabs_changed(window, cx);
    }

    fn finish_open(
        &mut self,
        id: TabId,
        path: PathBuf,
        result: Result<RepoHandle, Arc<GitError>>,
        on_done: Option<OnCloneDone>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let restoring = self.restoring.contains(&id);
        self.restoring.retain(|slot| *slot != id);
        let Some(ix) = self.tabs.iter().position(|s| s.id == id) else {
            self.restore_finished(restoring, cx);
            if let Some(on_done) = on_done {
                on_done(result.map(drop), window, cx);
            }
            return;
        };
        if let Some(on_done) = on_done {
            // Clone: the dialog reports the outcome (error banner, 09 §3.1).
            let created = self.clone_slots.contains(&id);
            self.clone_slots.retain(|slot| *slot != id);
            let done = match result {
                Ok(handle) => {
                    self.install_repo(ix, id, path, handle, window, cx);
                    Ok(())
                }
                Err(err) if created => {
                    self.remove_tab(id, window, cx);
                    Err(err)
                }
                Err(err) => {
                    self.tabs[ix].content = self.welcome_content(window, cx);
                    Err(err)
                }
            };
            self.after_tabs_changed(window, cx);
            on_done(done, window, cx);
            return;
        }
        match result {
            Ok(handle) => self.install_repo(ix, id, path, handle, window, cx),
            Err(err)
                if restoring
                    && !matches!(
                        err.kind(),
                        GitErrorKind::UnsafeRepository
                            | GitErrorKind::Cancelled
                            | GitErrorKind::AuthCancelled
                    ) =>
            {
                // A tab of the last session whose repository is gone (for now): no "New tab" in its place, no
                // dialog of its own, and its path stays in `state.json` (see `persist_tabs`).
                self.tabs.remove(ix);
                if ix < self.active {
                    self.active -= 1;
                }
                if self.tabs.is_empty() {
                    self.push_welcome(window, cx);
                }
                self.unrestored.push((path, err.to_string()));
            }
            Err(err) if err.kind() == GitErrorKind::UnsafeRepository => {
                self.tabs[ix].content = self.welcome_content(window, cx);
                self.ask_trust(path, window, cx);
            }
            Err(err) => {
                self.tabs[ix].content = self.welcome_content(window, cx);
                if !matches!(
                    err.kind(),
                    GitErrorKind::Cancelled | GitErrorKind::AuthCancelled
                ) {
                    show_error(cx, present::open_failure(&err));
                }
            }
        }
        self.restore_finished(restoring, cx);
        self.after_tabs_changed(window, cx);
    }

    /// After a slot of the last session finished opening: once the last one did, one dialog names the repositories
    /// that could not be reopened.
    fn restore_finished(&self, was_restoring: bool, cx: &mut App) {
        if was_restoring && self.restoring.is_empty() && !self.unrestored.is_empty() {
            show_error(cx, present::restore_failures(&self.unrestored));
        }
    }

    /// Puts an opened repository into slot `ix` as a loading `RepoTab` and records it as recent.
    fn install_repo(
        &mut self,
        ix: usize,
        id: TabId,
        path: PathBuf,
        handle: RepoHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = handle.display_name();
        let workdir = handle.workdir().unwrap_or(path);
        let tab = cx.new(|cx| {
            RepoTab::new(
                id,
                TabBackend::Real(Arc::new(handle)),
                name,
                workdir.clone(),
                TabOptions::default(),
                window,
                cx,
            )
        });
        tab.update(cx, |tab, cx| {
            tab.reload(crate::backend::RefreshSet::ALL, cx)
        });
        if ix == self.active {
            tab.update(cx, |tab, cx| tab.focus(window, cx));
        }
        self.tabs[ix].content = TabContent::Repo(tab);
        state::update(cx, |s| s.push_recent(&workdir));
    }

    /// UnsafeRepository recovery: "Trust this repository?" → `safe.directory` → open again.
    fn ask_trust(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let mut request = crate::backend::prompt::ConfirmRequest::new(
            "Trust this repository?",
            format!(
                "Git refuses to work in '{}' because it is owned by someone else. Add it to Git's safe.directory list and open it?",
                path.display()
            ),
        );
        request.confirm_label = "Trust".into();
        crate::dialogs::confirm::open_with(
            window,
            cx,
            request,
            Box::new(move |yes, window, cx| {
                if !yes {
                    return;
                }
                let Some(rt) = crate::app::git_runtime(cx) else {
                    return;
                };
                let trust_path = path.clone();
                let task =
                    crate::backend::bg(cx, move || handle::add_safe_directory(&rt, &trust_path));
                let weak = weak.clone();
                window
                    .spawn(cx, async move |cx| {
                        let result = task.await;
                        weak.update_in(cx, |view, window, cx| match result {
                            Ok(()) => view.open_repository(path, true, window, cx),
                            Err(err) => show_error(
                                cx,
                                ErrorDialogData::new(
                                    "Could not trust the repository",
                                    err.to_string(),
                                ),
                            ),
                        })
                        .ok();
                    })
                    .detach();
            }),
        );
    }

    /// Adds an already created repository tab (the test harness, worktree "open in new tab" flows).
    pub fn add_repo_tab(
        &mut self,
        tab: Entity<RepoTab>,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = tab.read(cx).id();
        let ix = self.target_slot(select, window, cx);
        self.tabs[ix] = TabSlot {
            id,
            content: TabContent::Repo(tab),
        };
        self.after_tabs_changed(window, cx);
    }

    /// Closes the tab `id`; asks first when its running operation may stop half-way or its terminal reports a
    /// working shell (`close_questions`). The tab to its left becomes selected; the last tab is replaced by an empty
    /// one.
    pub fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|s| s.id == id) else {
            return;
        };
        let questions = self.close_questions(ix, cx);
        if questions.is_empty() {
            self.remove_tab(id, window, cx);
            return;
        }
        let weak = cx.weak_entity();
        ask_all(
            questions,
            window,
            cx,
            Box::new(move |window, cx| {
                weak.update(cx, |view, cx| view.remove_tab(id, window, cx))
                    .ok();
            }),
        );
    }

    fn remove_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|s| s.id == id) else {
            return;
        };
        let slot = self.tabs.remove(ix);
        if let TabContent::Repo(tab) = &slot.content {
            tab.update(cx, |tab, cx| tab.close(cx));
        }
        self.toasts.update(cx, |t, cx| t.clear_tab(id, cx));
        if self.tabs.is_empty() {
            self.unrestored.clear();
            self.push_welcome(window, cx);
            self.active = 0;
        } else if ix < self.active || (ix == self.active && self.active > 0) {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.after_tabs_changed(window, cx);
    }

    /// Selects tab `ix` (no-op out of range).
    pub fn select_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() || ix == self.active {
            return;
        }
        self.active = ix;
        self.after_tabs_changed(window, cx);
    }

    /// Moves tab `from` to position `to` (drag reorder); the moved tab keeps the selection if it had it.
    pub fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let slot = self.tabs.remove(from);
        self.tabs.insert(to, slot);
        self.active = self
            .tabs
            .iter()
            .position(|s| s.id == active_id)
            .unwrap_or(0);
        self.persist_tabs(cx);
        cx.notify();
    }

    fn persist_tabs(&self, cx: &mut Context<Self>) {
        let mut open = Vec::new();
        let mut selected = 0;
        for (ix, slot) in self.tabs.iter().enumerate() {
            if let Some(path) = slot.path(cx) {
                if ix == self.active {
                    selected = open.len();
                }
                open.push(path);
            }
        }
        open.extend(self.unrestored.iter().map(|(path, _)| path.clone()));
        let unchanged = cx
            .try_global::<state::UiStateGlobal>()
            .is_some_and(|s| s.0.tabs_open == open && s.0.selected_tab == selected);
        if !unchanged {
            state::update(cx, |s| {
                s.tabs_open = open;
                s.selected_tab = selected;
            });
        }
    }

    fn after_tabs_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        let active_tab = self.tabs.get(self.active);
        let tab_id = active_tab.filter(|s| s.repo().is_some()).map(|s| s.id);
        self.toasts.update(cx, |t, cx| t.set_active_tab(tab_id, cx));
        window.set_window_title(&self.window_title(cx));
        self.persist_tabs(cx);
        cx.notify();
    }

    fn schedule_bounds_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Windows parks a minimized window far off screen and reports its restore box as a floating window: saving
        // that would bring a maximized window back floating.
        if minimized(window.bounds()) {
            self._bounds_save = None;
            return;
        }
        let display = window
            .display(cx)
            .and_then(|display| display.uuid().ok())
            .map(|uuid| uuid.to_string());
        let placement = crate::app::window_state(window.window_bounds(), display);
        self._bounds_save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            cx.update(|cx| {
                let unchanged = cx
                    .try_global::<state::UiStateGlobal>()
                    .is_some_and(|s| s.0.window == placement);
                if !unchanged {
                    state::update(cx, |s| s.window = placement);
                }
            });
        }));
    }

    /// Answers a question of a background thread: selects the asking tab first (deviation 34), then opens the
    /// credential or confirmation dialog.
    fn handle_request(&mut self, request: UiRequest, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = request.tab()
            && let Some(ix) = self.tabs.iter().position(|s| s.id == tab)
        {
            self.select_tab(ix, window, cx);
        }
        match request {
            UiRequest::Credential { request, reply, .. } => {
                crate::dialogs::credentials::open(window, cx, request, reply)
            }
            UiRequest::Confirm { request, reply, .. } => {
                crate::dialogs::confirm::open(window, cx, request, reply)
            }
        }
    }

    /// Opens the next queued error dialog when no dialog is showing.
    fn pump_errors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = window.has_active_dialog(cx);
        if self.error_slot == ErrorSlot::Showing && !active {
            self.error_slot = ErrorSlot::Idle;
        }
        if self.error_slot != ErrorSlot::Idle || active {
            return;
        }
        let Some(data) = self.error_queue.pop_front() else {
            return;
        };
        self.error_slot = ErrorSlot::Opening;
        #[cfg(test)]
        {
            self.errors_opened.push(data.title.clone());
            let hook = data
                .hook
                .as_ref()
                .map(|h| format!("{} ({:?}): {}", h.hook, h.exit_code, h.output))
                .unwrap_or_default();
            self.error_texts.push(format!(
                "{}
{}
{}",
                data.message,
                data.details.clone().unwrap_or_default(),
                hook
            ));
        }
        let weak = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let on_close = {
                let weak = weak.clone();
                Box::new(move |_: &mut Window, cx: &mut App| {
                    weak.update(cx, |view, cx| {
                        view.error_slot = ErrorSlot::Idle;
                        cx.notify();
                    })
                    .ok();
                })
            };
            crate::dialogs::error::open(window, cx, data, on_close);
            weak.update(cx, |view, cx| {
                if view.error_slot == ErrorSlot::Opening {
                    view.error_slot = ErrorSlot::Showing;
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn on_open_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_tab() {
            let ctx = tab.read(cx).ctx().clone();
            crate::toolbar::open_popup::open(&ctx, window, cx);
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |view, window, cx| {
                view.open_repository(path, true, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn content(&self, cx: &App) -> AnyElement {
        match self.tabs.get(self.active).map(|s| &s.content) {
            Some(TabContent::Welcome(view)) => view.clone().into_any_element(),
            Some(TabContent::Opening { path, .. }) => {
                crate::shell::loading::loading_repository(path, cx)
            }
            Some(TabContent::Repo(tab)) => tab.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }
}

/// Whether a window at `bounds` is minimized (Windows moves a minimized window to -32000, -32000).
pub(crate) fn minimized(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> bool {
    bounds.origin.x.as_f32() <= -30_000. && bounds.origin.y.as_f32() <= -30_000.
}

/// Asks `questions` one after the other and runs `then` once every one was confirmed (at once without questions,
/// so a caller inside an entity's update runs its own action directly instead).
fn ask_all(
    mut questions: Vec<ConfirmRequest>,
    window: &mut Window,
    cx: &mut App,
    then: Box<dyn FnOnce(&mut Window, &mut App)>,
) {
    if questions.is_empty() {
        then(window, cx);
        return;
    }
    let first = questions.remove(0);
    crate::dialogs::confirm::open_with(
        window,
        cx,
        first,
        Box::new(move |yes, window, cx| {
            if yes {
                // The answered dialog closes in this update: the next one opens after it.
                window.defer(cx, move |window, cx| ask_all(questions, window, cx, then));
            }
        }),
    );
}

/// How a slot gets its repository.
enum OpenKind {
    Open,
    Init,
    Clone(
        Box<CloneOptions>,
        CancelToken,
        crate::backend::prompt::ProgressPump,
    ),
}

/// A running clone (`AppView::clone_repository`).
pub struct CloneJob {
    /// Cancels the clone (the dialog's Cancel button).
    pub cancel: CancelToken,
    /// Git's progress events (`backend::prompt::progress_text` formats them); the stream ends when the clone ended.
    pub progress: UnboundedReceiver<crate::backend::Progress>,
}

/// Runs on the UI thread when a clone ended: `Ok` once its tab is open, `Err` (also `Cancelled`) otherwise.
pub type OnCloneDone = Box<dyn FnOnce(Result<(), Arc<GitError>>, &mut Window, &mut App)>;

/// Waits for the Git runtime, then opens / inits / clones in the background.
async fn open_in_background(
    path: PathBuf,
    kind: OpenKind,
    cx: &mut AsyncWindowContext,
) -> Result<RepoHandle, Arc<GitError>> {
    let rt = loop {
        let status = cx
            .update(|_, cx| crate::app::git_status(cx))
            .map_err(|e| Arc::new(GitError::Other(e)))?;
        match status {
            GitStatus::Ready(rt) => break rt,
            GitStatus::Failed(err) => return Err(err),
            GitStatus::Starting => {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await
            }
        }
    };
    let (undo, prompter) = cx
        .update(|_, cx| {
            let prompter: Arc<dyn crate::backend::CredentialPrompter> =
                match crate::app::ui_requests(cx) {
                    Some(tx) => Arc::new(crate::backend::prompt::GuiPrompter { tx, tab: None }),
                    None => Arc::new(crate::backend::NoPrompter),
                };
            (crate::app::undo_settings(cx), prompter)
        })
        .map_err(|e| Arc::new(GitError::Other(e)))?;
    cx.background_spawn(async move {
        match kind {
            OpenKind::Open => handle::open_repo(&rt, &path, &undo),
            OpenKind::Init => handle::init_repo(&rt, &path, &undo),
            OpenKind::Clone(opts, cancel, pump) => {
                // The context owns the only sender: the dialog's progress stream ends with this closure.
                let ctx = crate::backend::OpContext {
                    cancel,
                    progress: Arc::new(pump),
                    prompter,
                    label: format!("Clone {}", opts.url),
                };
                crate::backend::validate::clone_repository(&rt, &ctx, &opts)
                    .map(|outcome| handle::handle_for(outcome.value, &undo))
            }
        }
        .map_err(Arc::new)
    })
    .await
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.pump_errors(window, cx);
        let tokens = *cx.tokens();
        let content = self.content(cx);
        observed(ids::APP_ROOT)
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .text_color(tokens.on_background)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .key_context(APP_CONTEXT)
                    .track_focus(&self.focus)
                    .on_action(
                        cx.listener(|view, _: &NewTab, window, cx| view.new_empty_tab(window, cx)),
                    )
                    .on_action(cx.listener(|view, _: &CloseTab, window, cx| {
                        if let Some(slot) = view.tabs.get(view.active) {
                            let id = slot.id;
                            view.close_tab(id, window, cx);
                        }
                    }))
                    .on_action(cx.listener(|view, _: &PrevTab, window, cx| {
                        if view.active > 0 {
                            view.select_tab(view.active - 1, window, cx);
                        }
                    }))
                    .on_action(cx.listener(|view, _: &NextTab, window, cx| {
                        view.select_tab(view.active + 1, window, cx);
                    }))
                    .on_action(cx.listener(|view, _: &OpenRepository, window, cx| {
                        view.on_open_repository(window, cx)
                    }))
                    .on_action(cx.listener(|_, _: &OpenSettings, window, cx| {
                        crate::dialogs::settings::open(window, cx, None)
                    }))
                    .child(self.tab_strip.clone())
                    .child(
                        observed(ids::TAB_AREA)
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(content)
                            // An absolute element without insets sits at its static position (after the
                            // content), so the toast layer gets an inset wrapper over the tab area.
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .child(self.toasts.clone()),
                            ),
                    ),
            )
            .child(self.menus.clone())
    }
}

/// Whether `path` is the directory of an open tab (worktree flows).
pub fn is_open(view: &AppView, path: &Path, cx: &App) -> bool {
    view.tabs
        .iter()
        .any(|s| s.path(cx).as_deref() == Some(path))
}
