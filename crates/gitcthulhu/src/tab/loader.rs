//! Category loaders (spec 21 §2.6): `RepoTab::reload(RefreshSet)` unions the request into the pending set and
//! starts one background read per category.
//!
//! Coalescing: STATUS, REPO_STATE, the ref categories (one `refs::list`), SUBMODULES, WORKTREES, CONFIG and UNDO
//! cancel and restart (latest wins, `ReadSlot`). LOG uses a dirty flag: a LOG request while a LOG load runs only
//! marks it dirty and a new load starts when the running one has been applied (a cancelled `LogWalker::new` keeps
//! a thread busy, so LOG loads are cancelled only when the tab closes). Watcher-driven reloads of a window that has
//! been inactive for more than 2 s are deferred until it is activated again.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, Context, SharedString, Task};
use parking_lot::Mutex;

use super::model::{ChunkedRows, LogSnapshot, REFS};
use super::read::ReadSlot;
use super::{RepoModelEvent, RepoTab, RepoTabEvent};
use crate::backend::handle::TabBackend;
use crate::backend::log::LogPager;
use crate::backend::{CancelToken, GitError, ObjectId, OpContext, RefreshSet, git, loaders};
use crate::notifications::ToastKind;

/// The first log load reads at least this many commits.
pub const FIRST_LOG_ROWS: usize = 2000;
/// Watcher reloads of a window inactive for longer than this are deferred.
pub const INACTIVE_DEFER: Duration = Duration::from_secs(2);

/// Loader state of a tab.
#[derive(Default)]
pub struct Loader {
    status: ReadSlot,
    state: ReadSlot,
    refs: ReadSlot,
    submodules: ReadSlot,
    worktrees: ReadSlot,
    config: ReadSlot,
    undo: ReadSlot,
    ahead_behind: ReadSlot,
    /// Pairs requested by the running ahead/behind read.
    ahead_behind_requested: Vec<(ObjectId, ObjectId)>,
    log_task: Option<Task<()>>,
    log_cancel: Option<CancelToken>,
    log_dirty: bool,
    log_generation: u64,
    index_refresh: Option<Task<()>>,
    /// The commit-graph check of this session has been started (it runs once per tab).
    commit_graph_checked: bool,
    commit_graph_task: Option<Task<()>>,
    commit_graph_cancel: Option<CancelToken>,
    last_index_refresh: Option<crate::backend::FileStamp>,
    /// Watcher-driven requests waiting for the window to become active.
    pub(crate) deferred: RefreshSet,
    /// When the window became inactive.
    pub(crate) inactive_since: Option<Instant>,
}

impl Loader {
    /// A LOG load is running.
    pub fn log_loading(&self) -> bool {
        self.log_task.is_some()
    }

    /// A LOG request arrived while a LOG load was running.
    pub fn log_dirty(&self) -> bool {
        self.log_dirty
    }

    /// The once-per-session commit-graph check has run or is running.
    pub fn commit_graph_checked(&self) -> bool {
        self.commit_graph_checked
    }

    /// The generation of the newest log load.
    pub fn log_generation(&self) -> u64 {
        self.log_generation
    }

    /// Cancels every read, including a running LOG load (tab closed).
    pub(crate) fn cancel_all(&mut self) {
        for slot in [
            &mut self.status,
            &mut self.state,
            &mut self.refs,
            &mut self.submodules,
            &mut self.worktrees,
            &mut self.config,
            &mut self.undo,
            &mut self.ahead_behind,
        ] {
            slot.cancel();
        }
        if let Some(cancel) = self.log_cancel.take() {
            cancel.cancel();
        }
        if let Some(cancel) = self.commit_graph_cancel.take() {
            cancel.cancel();
        }
        self.commit_graph_task = None;
        self.log_task = None;
        self.log_dirty = false;
        self.index_refresh = None;
    }
}

/// The 09 §3.17-style toast title of a failed category load.
fn failure_title(what: RefreshSet) -> &'static str {
    if what.contains(RefreshSet::STATUS) {
        "Refreshing the status failed"
    } else if what.contains(RefreshSet::REPO_STATE) {
        "Refreshing the repository state failed"
    } else if what.intersects(REFS) {
        "Refreshing the branches failed"
    } else if what.contains(RefreshSet::LOG) {
        "Refreshing the log failed"
    } else if what.contains(RefreshSet::SUBMODULES) {
        "Refreshing the submodules failed"
    } else if what.contains(RefreshSet::WORKTREES) {
        "Refreshing the worktrees failed"
    } else if what.contains(RefreshSet::CONFIG) {
        "Refreshing the configuration failed"
    } else {
        "Refreshing the undo history failed"
    }
}

impl RepoTab {
    /// Reloads `what` now (operation-driven and manual reloads always run).
    pub fn reload(&mut self, what: RefreshSet, cx: &mut Context<Self>) {
        if what.is_empty() || self.closed {
            return;
        }
        match &self.ctx.backend {
            TabBackend::Real(_) => self.pump(what, cx),
            #[cfg(test)]
            TabBackend::Inert(inert) => inert.record(crate::test_support::TestCall::Reload(what)),
        }
    }

    /// Reloads `what` for a watcher event: deferred while the window has been inactive for more than 2 s.
    pub fn reload_from_watcher(&mut self, what: RefreshSet, cx: &mut Context<Self>) {
        if self
            .loader
            .inactive_since
            .is_some_and(|since| since.elapsed() > INACTIVE_DEFER)
        {
            self.loader.deferred |= what;
            return;
        }
        self.reload(what, cx);
    }

    /// Loads ahead/behind for the (local, tracking) pairs not in the model yet. A new request cancels the running
    /// one and requests the union of the still-missing pairs. Callers: the side panel, for visible rows only.
    pub fn request_ahead_behind(
        &mut self,
        pairs: Vec<(ObjectId, ObjectId)>,
        cx: &mut Context<Self>,
    ) {
        let loaded = self.ctx.model.read(cx).ahead_behind.clone();
        let mut missing: Vec<(ObjectId, ObjectId)> = Vec::new();
        for pair in self
            .loader
            .ahead_behind_requested
            .iter()
            .chain(pairs.iter())
        {
            if !loaded.contains_key(pair) && !missing.contains(pair) {
                missing.push(*pair);
            }
        }
        let new = pairs.iter().filter(|p| missing.contains(p)).count();
        if new == 0 && self.loader.ahead_behind.is_running() {
            return;
        }
        if missing.is_empty() {
            return;
        }
        let repo = match &self.ctx.backend {
            TabBackend::Real(handle) => handle.repo.clone(),
            #[cfg(test)]
            TabBackend::Inert(inert) => {
                inert.record(crate::test_support::TestCall::AheadBehind(missing.len()));
                return;
            }
        };
        self.loader.ahead_behind_requested = missing.clone();
        let request = missing.clone();
        self.loader.ahead_behind.start(
            cx,
            |t| &mut t.loader.ahead_behind,
            move |cancel| loaders::ahead_behind(&repo, &request, cancel),
            move |tab, result, cx| {
                tab.loader.ahead_behind_requested.clear();
                match result {
                    Ok(values) => tab.ctx.model.update(cx, |m, cx| {
                        let map = Arc::make_mut(&mut m.ahead_behind);
                        for (pair, value) in missing.iter().zip(values) {
                            map.insert(*pair, value);
                        }
                        cx.emit(RepoModelEvent(RefreshSet::BRANCHES));
                        cx.notify();
                    }),
                    Err(err) => tracing::warn!("ahead/behind failed: {err}"),
                }
            },
        );
    }

    fn pump(&mut self, what: RefreshSet, cx: &mut Context<Self>) {
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            return;
        };
        let repo = handle.repo.clone();
        if what.contains(RefreshSet::STATUS) {
            let repo = repo.clone();
            self.ctx.model.update(cx, |m, _| m.status.loading = true);
            self.loader.status.start(
                cx,
                |t| &mut t.loader.status,
                move |cancel| loaders::status(&repo, cancel),
                |tab, result, cx| tab.apply_status(result, cx),
            );
        }
        if what.contains(RefreshSet::REPO_STATE) {
            let repo = repo.clone();
            self.ctx.model.update(cx, |m, _| m.state.loading = true);
            self.loader.state.start(
                cx,
                |t| &mut t.loader.state,
                move |_| {
                    let state = loaders::repo_state(&repo)?;
                    let draft = loaders::load_draft(&repo).unwrap_or_else(|err| {
                        tracing::warn!("cannot read the commit draft: {err}");
                        None
                    });
                    Ok::<_, GitError>((state, draft))
                },
                |tab, result, cx| {
                    let ok = result.is_ok();
                    let error = result.as_ref().err().map(|e| e.to_string());
                    tab.ctx.model.update(cx, |m, cx| {
                        match result {
                            Ok((state, draft)) => {
                                m.state.apply::<GitError>(Ok(Arc::new(state)));
                                m.draft_message = draft;
                            }
                            Err(err) => m.state.apply::<GitError>(Err(err)),
                        }
                        cx.emit(RepoModelEvent(RefreshSet::REPO_STATE));
                        cx.notify();
                    });
                    if !ok {
                        tab.load_failed(RefreshSet::REPO_STATE, error, cx);
                    }
                },
            );
        }
        if what.intersects(REFS) {
            let repo = repo.clone();
            self.ctx.model.update(cx, |m, _| m.refs.loading = true);
            self.loader.refs.start(
                cx,
                |t| &mut t.loader.refs,
                move |_| loaders::refs(&repo),
                |tab, result, cx| {
                    let error = result.as_ref().err().map(|e| e.to_string());
                    tab.ctx.model.update(cx, |m, cx| {
                        m.refs.apply(result.map(Arc::new));
                        let pairs = m.tracking_pairs();
                        if m.ahead_behind.keys().any(|k| !pairs.contains(k)) {
                            let kept: HashMap<_, _> = m
                                .ahead_behind
                                .iter()
                                .filter(|(k, _)| pairs.contains(k))
                                .map(|(k, v)| (*k, *v))
                                .collect();
                            m.ahead_behind = Arc::new(kept);
                        }
                        cx.emit(RepoModelEvent(REFS));
                        cx.notify();
                    });
                    if error.is_some() {
                        tab.load_failed(REFS, error, cx);
                    }
                },
            );
        }
        if what.contains(RefreshSet::LOG) {
            self.start_log(cx);
        }
        if what.contains(RefreshSet::SUBMODULES) {
            let repo = repo.clone();
            self.ctx
                .model
                .update(cx, |m, _| m.submodules.loading = true);
            self.loader.submodules.start(
                cx,
                |t| &mut t.loader.submodules,
                move |cancel| loaders::submodules(&repo, cancel),
                |tab, result, cx| {
                    let error = result.as_ref().err().map(|e| e.to_string());
                    tab.ctx.model.update(cx, |m, cx| {
                        m.submodules.apply(result.map(Arc::new));
                        cx.emit(RepoModelEvent(RefreshSet::SUBMODULES));
                        cx.notify();
                    });
                    if error.is_some() {
                        tab.load_failed(RefreshSet::SUBMODULES, error, cx);
                    }
                },
            );
        }
        if what.contains(RefreshSet::WORKTREES) {
            let repo = repo.clone();
            self.ctx.model.update(cx, |m, _| m.worktrees.loading = true);
            self.loader.worktrees.start(
                cx,
                |t| &mut t.loader.worktrees,
                move |_| loaders::worktrees(&repo),
                |tab, result, cx| {
                    let error = result.as_ref().err().map(|e| e.to_string());
                    tab.ctx.model.update(cx, |m, cx| {
                        m.worktrees.apply(result.map(Arc::new));
                        cx.emit(RepoModelEvent(RefreshSet::WORKTREES));
                        cx.notify();
                    });
                    if error.is_some() {
                        tab.load_failed(RefreshSet::WORKTREES, error, cx);
                    }
                },
            );
        }
        if what.contains(RefreshSet::CONFIG) {
            self.ctx.model.update(cx, |m, _| m.identity.loading = true);
            self.loader.config.start(
                cx,
                |t| &mut t.loader.config,
                move |_| {
                    loaders::reload_config(&repo)?;
                    loaders::identity(Some(&repo))
                },
                |tab, result, cx| {
                    let error = result.as_ref().err().map(|e| e.to_string());
                    tab.ctx.model.update(cx, |m, cx| {
                        m.identity.apply(result.map(Arc::new));
                        cx.emit(RepoModelEvent(RefreshSet::CONFIG));
                        cx.notify();
                    });
                    if error.is_some() {
                        tab.load_failed(RefreshSet::CONFIG, error, cx);
                    }
                },
            );
        }
        if what.contains(RefreshSet::UNDO) {
            self.loader.undo.start(
                cx,
                |t| &mut t.loader.undo,
                move |_| {
                    handle
                        .undo_state()
                        .map(|state| (state, handle.oplog_unavailable.clone()))
                },
                |tab, result, cx| {
                    tab.ctx.model.update(cx, |m, cx| {
                        match result {
                            Ok((state, unavailable)) => {
                                m.undo_unavailable =
                                    if state.is_none() { unavailable } else { None };
                                m.undo = state;
                            }
                            Err(err) => {
                                m.undo = None;
                                m.undo_unavailable = Some(err);
                            }
                        }
                        cx.emit(RepoModelEvent(RefreshSet::UNDO));
                        cx.notify();
                    });
                },
            );
        }
    }

    fn apply_status(
        &mut self,
        result: Result<crate::backend::Status, GitError>,
        cx: &mut Context<Self>,
    ) {
        let error = result.as_ref().err().map(|e| e.to_string());
        let needs_refresh = result
            .as_ref()
            .ok()
            .filter(|s| s.index_needs_refresh)
            .map(|s| s.fingerprint.index);
        self.ctx.model.update(cx, |m, cx| {
            m.status.apply(result.map(Arc::new));
            cx.emit(RepoModelEvent(RefreshSet::STATUS));
            cx.notify();
        });
        if error.is_some() {
            self.load_failed(RefreshSet::STATUS, error, cx);
            return;
        }
        let status = self.ctx.model.read(cx).status.value.clone();
        self.ctx
            .ui
            .update(cx, |ui, cx| ui.prune_paths(status.as_deref(), cx));
        self.emit(RepoTabEvent::StatusReloaded, cx);
        if let Some(index_stamp) = needs_refresh {
            self.refresh_index(index_stamp, cx);
        }
    }

    /// 20 §2.5: once per status that reports stale index stat data, and only while no operation runs, refresh the
    /// index in the background (silently skipped when the guard is held) and tell the watcher to ignore the
    /// resulting index change.
    fn refresh_index(
        &mut self,
        index_stamp: Option<crate::backend::FileStamp>,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() || self.loader.index_refresh.is_some() {
            return;
        }
        if index_stamp.is_some() && self.loader.last_index_refresh == index_stamp {
            return;
        }
        self.loader.last_index_refresh = index_stamp;
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            return;
        };
        let bg = cx.background_spawn(async move {
            match loaders::refresh_index(&handle, &CancelToken::new()) {
                Ok(Some(stamp)) => handle.ignore_index_stamp(stamp),
                Ok(None) => {}
                Err(err) => tracing::debug!("index refresh skipped: {err}"),
            }
        });
        self.loader.index_refresh = Some(cx.spawn(async move |this, cx| {
            bg.await;
            this.update(cx, |tab, _| tab.loader.index_refresh = None)
                .ok();
        }));
    }

    fn start_log(&mut self, cx: &mut Context<Self>) {
        if self.loader.log_task.is_some() {
            self.loader.log_dirty = true;
            return;
        }
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            return;
        };
        let previously = self
            .ctx
            .model
            .read(cx)
            .log
            .value
            .as_ref()
            .map_or(0, |s| s.rows.len());
        let rows = previously.max(FIRST_LOG_ROWS);
        self.loader.log_generation += 1;
        let generation = self.loader.log_generation;
        let cancel = CancelToken::new();
        self.loader.log_cancel = Some(cancel.clone());
        self.ctx.model.update(cx, |m, _| m.log.loading = true);
        // The snapshot is built here too: moving 100k rows into chunks takes milliseconds.
        let bg = cx.background_spawn(async move {
            let mut pager = LogPager::new(&handle.repo, &cancel)?;
            let (rows, max_lane, exhausted) = pager.next_rows(rows, &cancel)?;
            let snapshot = LogSnapshot {
                rows: ChunkedRows::new().appended(rows),
                max_lane,
                exhausted,
                generation,
            };
            Ok::<_, GitError>((pager, snapshot))
        });
        self.loader.log_task = Some(cx.spawn(async move |this, cx| {
            let result = bg.await;
            this.update(cx, |tab, cx| tab.apply_log(generation, result, cx))
                .ok();
        }));
    }

    fn apply_log(
        &mut self,
        generation: u64,
        result: Result<(LogPager, LogSnapshot), GitError>,
        cx: &mut Context<Self>,
    ) {
        self.loader.log_task = None;
        self.loader.log_cancel = None;
        match result {
            Ok((pager, snapshot)) => {
                let exhausted = snapshot.exhausted;
                let previous = self.ctx.model.update(cx, |m, cx| {
                    m.log_pager = Some(Arc::new(Mutex::new(pager)));
                    m.set_log(Arc::new(snapshot), cx)
                });
                // Freeing a deep log's rows takes milliseconds: off the UI thread (a no-op while a view still holds
                // the snapshot, which then frees it).
                if let Some(previous) = previous {
                    cx.background_spawn(async move { drop(previous) }).detach();
                }
                self.emit(RepoTabEvent::LogReloaded { generation }, cx);
                self.start_commit_graph(exhausted, git::config::COMMIT_GRAPH_THRESHOLD, cx);
            }
            Err(err) => {
                let text = err.to_string();
                self.ctx.model.update(cx, |m, cx| {
                    m.log.apply::<GitError>(Err(err));
                    cx.emit(RepoModelEvent(RefreshSet::LOG));
                    cx.notify();
                });
                self.load_failed(RefreshSet::LOG, Some(text), cx);
            }
        }
        if std::mem::take(&mut self.loader.log_dirty) {
            self.start_log(cx);
        }
    }

    /// After the first LOG load that did not reach the end of the history: once per tab, writes the commit-graph
    /// in the background when the repository has none and more than `threshold` reachable commits, so that the
    /// next log, ahead/behind and file-history reads are fast. It takes no operation guard and shows nothing;
    /// closing the tab cancels it.
    pub(crate) fn start_commit_graph(
        &mut self,
        exhausted: bool,
        threshold: usize,
        cx: &mut Context<Self>,
    ) {
        if exhausted || self.loader.commit_graph_checked {
            return;
        }
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            return;
        };
        self.loader.commit_graph_checked = true;
        let cancel = CancelToken::new();
        self.loader.commit_graph_cancel = Some(cancel.clone());
        let bg = cx.background_spawn(async move {
            let ctx = OpContext::silent(cancel);
            git::config::write_commit_graph_if_large(&handle.repo, threshold, &ctx)
        });
        self.loader.commit_graph_task = Some(cx.spawn(async move |this, cx| {
            let result = bg.await;
            this.update(cx, |tab, _| {
                tab.loader.commit_graph_task = None;
                tab.loader.commit_graph_cancel = None;
            })
            .ok();
            match result {
                Ok(true) => tracing::info!("wrote the commit-graph of a large repository"),
                Ok(false) => {}
                Err(err) if err.kind() == crate::backend::GitErrorKind::Cancelled => {}
                Err(err) => tracing::warn!("cannot write the commit-graph: {err}"),
            }
        }));
    }

    fn load_failed(&mut self, what: RefreshSet, error: Option<String>, cx: &mut Context<Self>) {
        let title = failure_title(what);
        let text: SharedString = match error {
            Some(error) => format!("{title}: {error}").into(),
            None => title.into(),
        };
        tracing::warn!("{text}");
        crate::app_view::show_toast(cx, Some(self.ctx.id), ToastKind::Error, text, None);
    }
}
