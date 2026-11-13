//! Repository tabs (spec 21 §2.1): the `RepoTab` controller entity, the `TabCtx` handed to every region view, and
//! the tab events.
//!
//! Re-entrancy rules (probe `tab_updating_child_that_reads_tab_panics`):
//! 1. Region views get a [`TabCtx`] and read data only from `RepoModel` / `RepoUi` (and `TabCtx::backend` for their
//!    own reads). They never `read`/`update` the `RepoTab` entity for data; they call `ctx.tab.update(..)` only from
//!    their own event handlers (click, key, drop) to start operations or reloads.
//! 2. `RepoTab` never updates a view synchronously: it emits [`RepoTabEvent`]s through `ctx.events` or defers.
//! 3. `then` / `on_error` / `retry` closures of operations run inside the tab's update: they may change `RepoTab`,
//!    `RepoUi`, `RepoModel`, open dialogs and emit events, but never update region views directly.
//! 4. Views subscribe to `RepoModelEvent` and notify only when the event intersects their categories.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, SharedString, Subscription,
    Task, WeakEntity, Window,
};

use crate::backend::handle::{TabBackend, UndoSettings};
use crate::backend::prompt::{Asker, GuiPrompter};
use crate::backend::{CredentialPrompter, NoPrompter, ObjectId, RefreshSet, RelaPath};
use crate::widgets::split::SplitWidths;

pub mod loader;
pub mod model;
pub mod ops;
pub mod read;
pub mod repo_view;
pub mod ui;
pub mod undo_settings;

pub use loader::Loader;
pub use model::{RepoModel, RepoModelEvent};
pub use ops::{BusyState, OpKind};
pub use read::ReadSlot;
pub use ui::{RepoUi, RepoUiEvent};

/// Element ids of the repository frame owned by the tab (the shell ids are in `crate::ids`).
pub mod ids {
    /// The processing overlay container (07 §3.2).
    pub const PROCESSING_OVERLAY: &str = "processing-overlay";
    /// The centre pane's log slot.
    pub const CENTER_LOG: &str = "center-log";
    /// The centre pane's diff slot.
    pub const CENTER_DIFF: &str = "center-diff";
}

/// Identifies a repository tab for the lifetime of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl TabId {
    /// A new, process-unique id.
    pub fn next() -> TabId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TabId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Everything a region view needs. Cheap to clone; handed out at view construction.
#[derive(Clone)]
pub struct TabCtx {
    pub id: TabId,
    /// The repository (or the inert test backend).
    pub backend: TabBackend,
    pub model: Entity<RepoModel>,
    pub ui: Entity<RepoUi>,
    /// Emitter only: views subscribe here, never to `RepoTab`.
    pub events: Entity<TabEvents>,
    /// For calling operations from event handlers only.
    pub tab: WeakEntity<RepoTab>,
}

impl TabCtx {
    /// Starts an operation or reload on the tab from an event handler: `f` runs inside `tab.update`. A no-op when
    /// the tab is gone. Never call this from a render function or from inside the tab's own update.
    pub fn with_tab(
        &self,
        window: &mut Window,
        cx: &mut App,
        f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>),
    ) {
        if let Some(tab) = self.tab.upgrade() {
            tab.update(cx, |tab, cx| f(tab, window, cx));
        }
    }
}

/// The tab's event emitter.
pub struct TabEvents;

impl EventEmitter<RepoTabEvent> for TabEvents {}

/// Events of a tab, emitted through [`TabCtx::events`].
#[derive(Clone, Debug, PartialEq)]
pub enum RepoTabEvent {
    /// The status was reloaded (the diff view reloads if it shows an uncommitted file).
    StatusReloaded,
    /// A new log snapshot (new pager generation).
    LogReloaded {
        generation: u64,
    },
    /// The log scrolls to the commit if its row is not visible (08 §9.5 fix).
    ScrollToCommit {
        id: ObjectId,
    },
    OpStarted {
        kind: OpKind,
    },
    OpFinished {
        kind: OpKind,
        ok: bool,
    },
    FocusCommitMessage,
    ReloadDiff,
}

/// How a tab talks to the user from background threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrompterMode {
    /// Credential and confirmation dialogs through the window's request pump.
    Gui,
    /// `NoPrompter` + `Asker::Auto(false)`: fixture tests (nothing may block on the UI there).
    Refuse,
}

/// Options of a new tab.
#[derive(Clone, Copy, Debug)]
pub struct TabOptions {
    /// Start the repository watcher (default true; false in unit tests, where a watcher thread would feed events
    /// into the deterministic test scheduler).
    pub watch: bool,
    pub prompter: PrompterMode,
}

impl Default for TabOptions {
    fn default() -> Self {
        Self {
            watch: !cfg!(test),
            prompter: PrompterMode::Gui,
        }
    }
}

/// The region views of a tab (spec 21 §2.1). Blame, file history and the rebase editor exist only while shown.
pub struct RegionViews {
    pub toolbar: Entity<crate::toolbar::ToolbarView>,
    pub side_panel: Entity<crate::side_panel::SidePanelView>,
    pub log: Entity<crate::log_view::LogView>,
    pub status: Entity<crate::status_pane::StatusPaneView>,
    pub commit_changes: Entity<crate::commit_changes::CommitChangesView>,
    pub diff: Entity<crate::diff::DiffView>,
    pub blame: Option<Entity<crate::blame::BlameView>>,
    pub history: Option<(RelaPath, Entity<crate::file_history::FileHistoryView>)>,
    pub rebase: Option<Entity<crate::rebase_interactive::RebaseInteractiveView>>,
    pub bottom_bar: Entity<crate::shell::bottom_bar::BottomBarView>,
    // D0 entities (milestone 2 fills them).
    pub undo: Entity<crate::undo::UndoController>,
    pub ai: Entity<crate::ai::AiController>,
    pub forge: Option<Entity<crate::forge::ForgeController>>,
    pub terminal: Option<Entity<crate::terminal::TerminalPanel>>,
}

/// The controller of one repository tab: operation runner, loaders, watcher; owns the model, the UI state and the
/// region views. Rendered as the repository frame (`repo_view.rs`).
pub struct RepoTab {
    pub(crate) ctx: TabCtx,
    name: SharedString,
    path: PathBuf,
    options: TabOptions,
    pub(crate) asker: Asker,
    pub(crate) prompter: Arc<dyn CredentialPrompter>,
    pub(crate) busy: Option<BusyState>,
    /// Increments per operation; stale overlay timers and confirmations compare against it.
    pub(crate) op_seq: u64,
    pub(crate) op_task: Option<Task<()>>,
    pub(crate) overlay_task: Option<Task<()>>,
    pub(crate) progress_task: Option<Task<()>>,
    pub(crate) loader: Loader,
    pub(crate) undo: undo_settings::UndoState,
    pub(crate) views: RegionViews,
    pub(crate) focus: FocusHandle,
    pub(crate) overlay_focus: FocusHandle,
    /// Pane widths while a gutter is dragged (otherwise the `UiState` widths apply).
    pub(crate) drag_widths: Option<SplitWidths>,
    pub(crate) watcher_task: Option<Task<()>>,
    pub(crate) watch_failed: bool,
    pub(crate) closed: bool,
    _subscriptions: Vec<Subscription>,
}

impl RepoTab {
    /// A tab over `backend`. Does not load anything: the caller starts `reload(RefreshSet::ALL)` (AppView, test
    /// harness).
    pub fn new(
        id: TabId,
        backend: TabBackend,
        name: SharedString,
        path: PathBuf,
        options: TabOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = cx.new(|_| RepoModel::default());
        let ui = cx.new(|_| RepoUi::new());
        let events = cx.new(|_| TabEvents);
        let ctx = TabCtx {
            id,
            backend,
            model: model.clone(),
            ui: ui.clone(),
            events,
            tab: cx.weak_entity(),
        };
        let views = RegionViews {
            toolbar: cx.new(|cx| crate::toolbar::ToolbarView::new(ctx.clone(), window, cx)),
            side_panel: cx.new(|cx| crate::side_panel::SidePanelView::new(ctx.clone(), window, cx)),
            log: cx.new(|cx| crate::log_view::LogView::new(ctx.clone(), window, cx)),
            status: cx.new(|cx| crate::status_pane::StatusPaneView::new(ctx.clone(), window, cx)),
            commit_changes: cx
                .new(|cx| crate::commit_changes::CommitChangesView::new(ctx.clone(), window, cx)),
            diff: cx.new(|cx| crate::diff::DiffView::new(ctx.clone(), window, cx)),
            blame: None,
            history: None,
            rebase: None,
            bottom_bar: cx.new(|cx| {
                crate::shell::bottom_bar::BottomBarView::new(Some(ctx.clone()), window, cx)
            }),
            undo: cx.new(|cx| crate::undo::UndoController::new(ctx.clone(), cx)),
            ai: cx.new(|cx| crate::ai::AiController::new(ctx.clone(), cx)),
            forge: crate::forge::create_controller(&ctx, cx),
            terminal: crate::terminal::create_panel(&ctx, window, cx),
        };
        let (asker, prompter): (Asker, Arc<dyn CredentialPrompter>) = match options.prompter {
            PrompterMode::Refuse => (Asker::Auto(false), Arc::new(NoPrompter)),
            PrompterMode::Gui => match crate::app::ui_requests(cx) {
                Some(tx) => (
                    Asker::Ui {
                        tx: tx.clone(),
                        tab: Some(id),
                    },
                    Arc::new(GuiPrompter { tx, tab: Some(id) }),
                ),
                None => (Asker::Auto(false), Arc::new(NoPrompter)),
            },
        };
        let mut subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&ui, |_, _, cx| cx.notify()),
            undo_settings::observe(cx),
            cx.observe_window_activation(window, |tab, window, cx| {
                tab.on_window_activation(window.is_window_active(), cx)
            }),
        ];
        let focus = cx.focus_handle();
        // A dialog that closes hands the focus back into the frame: the overlay of a running operation takes it.
        subscriptions.push(cx.on_focus_in(&focus, window, |tab, window, cx| {
            tab.focus_overlay_if_free(window, cx)
        }));
        let mut tab = Self {
            ctx,
            name,
            path,
            options,
            asker,
            prompter,
            busy: None,
            op_seq: 0,
            op_task: None,
            overlay_task: None,
            progress_task: None,
            loader: Loader::default(),
            undo: undo_settings::UndoState::new(UndoSettings::from_settings(
                &crate::settings::get_or_default(cx),
            )),
            views,
            focus,
            overlay_focus: cx.focus_handle(),
            drag_widths: None,
            watcher_task: None,
            watch_failed: false,
            closed: false,
            _subscriptions: subscriptions,
        };
        if tab.options.watch {
            tab.start_watcher(cx);
        }
        tab
    }

    /// The context handed to region views.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The tab's id.
    pub fn id(&self) -> TabId {
        self.ctx.id
    }

    /// The tab title (07 §2.2).
    pub fn name(&self) -> SharedString {
        self.name.clone()
    }

    /// The repository directory the tab was opened with.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// The region views.
    pub fn views(&self) -> &RegionViews {
        &self.views
    }

    /// The asker of `prepare` closures (tests replace it through the harness).
    pub fn set_asker(&mut self, asker: Asker) {
        self.asker = asker;
    }

    /// Focuses the repository frame (after closing a closeable view, 09 §1.1).
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// Emits a tab event to the views (rule 2: never update a view directly).
    pub fn emit(&self, event: RepoTabEvent, cx: &mut Context<Self>) {
        self.ctx.events.update(cx, |_, cx| cx.emit(event));
    }

    /// Stops the watcher and cancels every read and the running operation (tab closed). Idempotent.
    ///
    /// An operation that cannot be cancelled (`Cancellable::No`) is left to finish in the background: killing it
    /// could leave the repository half-way through a checkout or rebase. A `WithWarning` one is cancelled, so the
    /// caller asks first (`AppView::close_tab`).
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(handle) = self.ctx.backend.handle() {
            handle.stop_watcher();
        }
        self.watcher_task = None;
        self.loader.cancel_all();
        if let Some(busy) = &self.busy
            && busy.cancellable != ops::Cancellable::No
        {
            busy.cancel.cancel();
        }
        cx.notify();
    }

    fn start_watcher(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        match handle.start_watcher(move |event| {
            let _ = tx.unbounded_send(event);
        }) {
            Ok(()) => {
                self.watcher_task = Some(cx.spawn(async move |this, cx| {
                    use futures::StreamExt as _;
                    while let Some(event) = rx.next().await {
                        if this
                            .update(cx, |tab, cx| tab.on_watch_event(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(err) => {
                tracing::warn!("cannot watch {}: {err}", self.path.display());
                self.watcher_failed(cx);
            }
        }
    }

    fn on_watch_event(&mut self, event: crate::backend::WatchEvent, cx: &mut Context<Self>) {
        match event {
            crate::backend::WatchEvent::Changed { refresh, .. } => {
                self.reload_from_watcher(refresh, cx)
            }
            crate::backend::WatchEvent::Error { message } => {
                tracing::warn!("watcher of {}: {message}", self.path.display());
                self.watcher_failed(cx);
            }
        }
    }

    /// Watching stopped working (or works only in part): one Error toast per tab, then refresh on window
    /// activation (spec 21 §2.6, drift 2.5: deduplicated).
    fn watcher_failed(&mut self, cx: &mut Context<Self>) {
        if self.watch_failed {
            return;
        }
        self.watch_failed = true;
        crate::app_view::show_toast(
            cx,
            Some(self.ctx.id),
            crate::notifications::ToastKind::Error,
            "Repository changes detection has stopped working",
            None,
        );
    }

    fn on_window_activation(&mut self, active: bool, cx: &mut Context<Self>) {
        if active {
            self.loader.inactive_since = None;
            let mut what = std::mem::take(&mut self.loader.deferred);
            if self.watch_failed {
                what |= RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::REPO_STATE;
            }
            if !what.is_empty() {
                self.reload(what, cx);
            }
        } else if self.loader.inactive_since.is_none() {
            self.loader.inactive_since = Some(std::time::Instant::now());
        }
    }
}

#[cfg(test)]
mod tests;
