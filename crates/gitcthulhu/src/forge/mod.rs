//! Forge integration (D0.1; owned by X4; spec 21 §5.5, §2.10, §2.13.3): the per-tab controller (detection, polling,
//! badge map), PR/CI badges, accounts, the branch and commit menu items and the pull request checkout.
//!
//! **Nothing here waits for the network on the UI thread.** The controller detects the forge of the repository
//! (no network), refreshes the review and check caches in the background (one tick per minute, every 5th tick
//! while the window is inactive, back-off after failures, a stop after errors only the user can fix) and then
//! computes the `branch -> badge` map from the caches, again in the background. Rows and chips only read that map.
//! Offline, or while the services start, the map stays as it was (empty at first). Once the detected forge has no
//! reviews or no connected account serves the repository (the account was forgotten), the map is cleared.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::{App, AppContext as _, Context, Entity, Global, Subscription, Task, WeakEntity};

use crate::backend::forge::{
    AccountMatch, BranchQuery, Failure, FailureKind, RepoFacts, RepoHandle, RepoPrefsInput,
    ReviewBadge,
};
use crate::backend::{ByteSlice as _, FullName};
use crate::tab::model::REFS;
use crate::tab::ops::OpKind;
use crate::tab::{ReadSlot, RepoModelEvent, RepoTabEvent, TabCtx};

pub mod accounts;
pub mod badges;
mod checkout;
pub mod flows;
mod menus;
pub mod poll;
#[cfg(feature = "qa")]
mod qa;

#[cfg(all(test, feature = "services"))]
mod tests;

pub use accounts::{AccountsEvent, AccountsModel};
pub use poll::{Poller, Timings};

/// A looked-up pull request for each local and remote-tracking branch that has one.
pub type BadgeMap = HashMap<FullName, ReviewBadge>;

/// How the forge of a tab stands (what the tooltips and the QA verbs report).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeStatus {
    /// The services or the repository are not available yet.
    Starting,
    /// The repository has no forge (or one without an API).
    NoForge,
    /// The repository is on a forge but no connected account serves it.
    NoAccount,
    /// Detected; nothing is running.
    Idle,
    /// A refresh from the forge is running.
    Refreshing,
    /// The forge could not be reached at the last refresh; the badges are from the cache.
    Offline,
    /// The rate limit was hit.
    RateLimited,
    /// The token was rejected or lacks a permission: polling stopped until the user acts.
    AuthFailed(String),
}

/// The per-tab forge controller.
pub struct ForgeController {
    ctx: TabCtx,
    handle: Option<RepoHandle>,
    badges: Arc<BadgeMap>,
    status: ForgeStatus,
    poller: Poller,
    detect_attempts: u32,
    refreshes: u64,
    last_error: Option<String>,
    detect: ReadSlot,
    refresh: ReadSlot,
    recompute: ReadSlot,
    /// Another recompute was requested while one was running.
    recompute_dirty: bool,
    /// A refresh was asked for (by the user, a push or an account change) while one was running: run another when it
    /// ends, so the answer reflects what happened after the running one started. The poll loop does not set it.
    refresh_dirty: bool,
    /// The accounts changed: refresh after the next detection even when the facts stay the same (a token was
    /// replaced after the forge rejected the old one).
    refresh_after_detect: bool,
    _loop: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The window state the cadence reads; tests replace it through [`ActivityOverride`].
#[derive(Clone)]
pub struct ActivityOverride(pub std::rc::Rc<std::cell::Cell<bool>>);

impl Global for ActivityOverride {}

fn window_active(cx: &App) -> bool {
    match cx.try_global::<ActivityOverride>() {
        Some(over) => over.0.get(),
        None => cx.active_window().is_some(),
    }
}

fn timings(cx: &App) -> Timings {
    cx.try_global::<poll::TimingsGlobal>()
        .map(|g| g.0)
        .unwrap_or_default()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The controllers of the open tabs, for the slot functions and menu extenders that only get a `TabCtx`.
#[derive(Default)]
struct Registry {
    controllers: HashMap<crate::tab::TabId, WeakEntity<ForgeController>>,
}

impl Global for Registry {}

/// The controller of `tab`, if the tab has one.
pub fn controller_of(tab: crate::tab::TabId, cx: &App) -> Option<Entity<ForgeController>> {
    cx.try_global::<Registry>()?
        .controllers
        .get(&tab)?
        .upgrade()
}

/// Creates the tab's forge controller. `None` in builds without the `services` feature.
pub fn create_controller(ctx: &TabCtx, cx: &mut App) -> Option<Entity<ForgeController>> {
    if !cfg!(feature = "services") {
        return None;
    }
    let tab_ctx = ctx.clone();
    let entity = cx.new(|cx| ForgeController::new(tab_ctx, cx));
    let registry = cx.default_global::<Registry>();
    registry
        .controllers
        .retain(|_, controller| controller.upgrade().is_some());
    registry.controllers.insert(ctx.id, entity.downgrade());
    Some(entity)
}

impl ForgeController {
    /// The controller of the tab of `ctx`: subscribes to the model, the tab's events and the accounts, and starts
    /// the polling loop.
    pub fn new(ctx: TabCtx, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![
            cx.subscribe(&ctx.model, |this, _, event: &RepoModelEvent, cx| {
                this.on_model(*event, cx)
            }),
            cx.subscribe(&ctx.events, |this, _, event: &RepoTabEvent, cx| {
                this.on_tab_event(event, cx)
            }),
        ];
        if let Some(model) = accounts::installed(cx) {
            subscriptions.push(cx.subscribe(&model, |this, _, _: &AccountsEvent, cx| {
                this.on_accounts_changed(cx)
            }));
        }
        let task = cx.spawn(async move |this, cx| {
            loop {
                let delay = match this.update(cx, |controller, cx| controller.step(cx)) {
                    Ok(delay) => delay,
                    Err(_) => return,
                };
                cx.background_executor().timer(delay).await;
            }
        });
        Self {
            ctx,
            handle: None,
            badges: Arc::new(BadgeMap::new()),
            status: ForgeStatus::Starting,
            poller: Poller::default(),
            detect_attempts: 0,
            refreshes: 0,
            last_error: None,
            detect: ReadSlot::default(),
            refresh: ReadSlot::default(),
            recompute: ReadSlot::default(),
            recompute_dirty: false,
            refresh_dirty: false,
            refresh_after_detect: false,
            _loop: Some(task),
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The pull request of a local or remote-tracking branch, if the last computation found one.
    pub fn badge(&self, branch: &FullName) -> Option<&ReviewBadge> {
        self.badges.get(branch)
    }

    /// The whole badge map.
    pub fn badges(&self) -> &BadgeMap {
        &self.badges
    }

    /// How the forge of the tab stands.
    pub fn status(&self) -> &ForgeStatus {
        &self.status
    }

    /// The forge could not be reached at the last refresh.
    pub fn offline(&self) -> bool {
        self.poller.offline()
    }

    /// What the app knows about the forge of the repository, once detected.
    pub fn facts(&self) -> Option<&RepoFacts> {
        self.handle.as_ref().map(RepoHandle::facts)
    }

    /// The repository's forge handle, once detected.
    pub fn handle(&self) -> Option<&RepoHandle> {
        self.handle.as_ref()
    }

    /// Whether detection, a refresh from the forge or a badge computation is running (QA idle probe, tests).
    pub fn is_busy(&self) -> bool {
        self.detect.is_running() || self.refresh.is_running() || self.recompute.is_running()
    }

    /// How many refreshes from the forge ran (tests, QA).
    pub fn refresh_count(&self) -> u64 {
        self.refreshes
    }

    /// The message of the last failure, if the last refresh failed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The cadence (tests).
    pub fn poller(&self) -> &Poller {
        &self.poller
    }

    /// Whether the repository's forge can list and create pull requests and a connected account serves it.
    pub fn can_use_reviews(&self) -> bool {
        self.facts().is_some_and(|facts| {
            facts.reviews && matches!(facts.account, AccountMatch::Matching(_))
        })
    }

    /// The user asked for fresh data (or did something about the accounts): ask the forge now.
    pub fn refresh_now(&mut self, cx: &mut Context<Self>) {
        self.poller.user_acted();
        self.request_refresh(cx);
    }

    /// Replaces the badge map (QA scenarios and tests inject data without a forge).
    pub fn set_badges(&mut self, badges: BadgeMap, cx: &mut Context<Self>) {
        if *self.badges != badges {
            self.badges = Arc::new(badges);
            cx.notify();
            cx.refresh_windows();
        }
    }

    // ---- the loop ------------------------------------------------------------------------------------------

    /// One step of the loop; returns how long to wait for the next one.
    fn step(&mut self, cx: &mut Context<Self>) -> Duration {
        let timings = timings(cx);
        if self.handle.is_none() {
            self.try_detect(false, cx);
            self.detect_attempts = self.detect_attempts.saturating_add(1);
            // The services start in the background and a repository may be inert: look often at first, then rarely.
            return if self.detect_attempts <= 20 {
                timings.detect_retry
            } else {
                Duration::from_secs(10).max(timings.detect_retry)
            };
        }
        if self.poller.due(window_active(cx), &timings) {
            self.start_refresh(cx);
        }
        timings.tick
    }

    /// Detects the forge (no network). `restart` replaces a running detection, so a change of the remotes or the
    /// accounts during one is never lost; the retry loop only starts one when none runs.
    fn try_detect(&mut self, restart: bool, cx: &mut Context<Self>) {
        if self.detect.is_running() && !restart {
            return;
        }
        let Some(services) = crate::app::services(cx) else {
            return;
        };
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        let prefs = repo_prefs(&repo, cx);
        self.detect.start(
            cx,
            |this| &mut this.detect,
            move |_cancel| crate::backend::forge::open_repo(&services, &repo, &prefs),
            |this, result, cx| this.apply_detection(result, cx),
        );
    }

    fn apply_detection(&mut self, result: Result<RepoHandle, Failure>, cx: &mut Context<Self>) {
        match result {
            Ok(handle) => {
                let changed = self.handle.as_ref().map(RepoHandle::facts) != Some(handle.facts());
                let refresh = std::mem::take(&mut self.refresh_after_detect) || changed;
                if changed {
                    // Unchanged facts keep the status (an auth failure stays visible until a refresh works).
                    self.status = status_of(handle.facts());
                }
                self.handle = Some(handle);
                if changed {
                    self.start_recompute(cx);
                }
                if refresh {
                    self.request_refresh(cx);
                }
            }
            Err(failure) => {
                self.status = ForgeStatus::Starting;
                self.last_error = Some(failure.message);
            }
        }
        cx.notify();
        cx.refresh_windows();
    }

    /// A refresh somebody asked for: when one is running it may predate what they did (a push, a new token), so
    /// another one follows it instead of the request being dropped.
    fn request_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_running() {
            tracing::debug!("forge refresh requested while one runs: queued");
            self.refresh_dirty = true;
            return;
        }
        self.start_refresh(cx);
    }

    fn start_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_running() {
            return;
        }
        let Some(handle) = self.handle.clone() else {
            tracing::debug!("forge refresh skipped: no detected forge yet");
            return;
        };
        if !handle.facts().reviews {
            tracing::debug!("forge refresh skipped: the forge has no reviews");
            return;
        }
        if !matches!(handle.facts().account, AccountMatch::Matching(_)) {
            tracing::debug!("forge refresh skipped: no connected account serves the repository");
            self.status = ForgeStatus::NoAccount;
            self.refresh_dirty = false;
            self.drop_badges(cx);
            return;
        }
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            tracing::debug!("forge refresh skipped: no repository");
            return;
        };
        let queries = self.queries(cx);
        self.status = ForgeStatus::Refreshing;
        self.refresh.start(
            cx,
            |this| &mut this.refresh,
            move |cancel| handle.refresh(&repo, &queries, cancel),
            |this, result, cx| this.apply_refresh(result, cx),
        );
    }

    fn apply_refresh(
        &mut self,
        result: Result<crate::backend::forge::RefreshReport, Failure>,
        cx: &mut Context<Self>,
    ) {
        self.refreshes += 1;
        match result {
            Ok(_) => {
                self.poller.succeeded();
                self.last_error = None;
                self.status = self
                    .handle
                    .as_ref()
                    .map_or(ForgeStatus::Starting, |h| status_of(h.facts()));
            }
            Err(failure) if failure.kind == FailureKind::Cancelled => {}
            Err(failure) => {
                let timings = timings(cx);
                self.poller.failed(&failure, now_unix(), &timings);
                self.status = match failure.kind {
                    FailureKind::RateLimited { .. } => ForgeStatus::RateLimited,
                    FailureKind::Auth | FailureKind::Permission => {
                        ForgeStatus::AuthFailed(failure.message.clone())
                    }
                    FailureKind::NoAccount => ForgeStatus::NoAccount,
                    _ if failure.may_pass() => ForgeStatus::Offline,
                    _ => ForgeStatus::Idle,
                };
                tracing::info!("forge refresh failed: {}", failure.message);
                self.last_error = Some(failure.message);
            }
        }
        // Whatever the answer, the cache may have changed (or is all there is while offline).
        self.start_recompute(cx);
        // A request made during this refresh runs now, except after a rate limit (the poller waits for the reset).
        if std::mem::take(&mut self.refresh_dirty) && self.status != ForgeStatus::RateLimited {
            self.start_refresh(cx);
        }
        cx.notify();
        cx.refresh_windows();
    }

    /// Forgets the badges and what is running for them: without a connected account the cached pull requests of the
    /// forge must not stay on the branches.
    fn drop_badges(&mut self, cx: &mut Context<Self>) {
        self.refresh.cancel();
        self.recompute.cancel();
        self.recompute_dirty = false;
        self.set_badges(BadgeMap::new(), cx);
    }

    fn start_recompute(&mut self, cx: &mut Context<Self>) {
        if self.recompute.is_running() {
            self.recompute_dirty = true;
            return;
        }
        let Some(handle) = self.handle.clone() else {
            return;
        };
        if !handle.facts().reviews || !matches!(handle.facts().account, AccountMatch::Matching(_)) {
            self.drop_badges(cx);
            return;
        }
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        let queries = self.queries(cx);
        self.recompute.start(
            cx,
            |this| &mut this.recompute,
            move |_cancel| handle.badges(&repo, &queries),
            |this, result, cx| {
                match result {
                    Ok(found) => {
                        let map: BadgeMap = found
                            .into_iter()
                            .filter_map(|(name, badge)| {
                                Some((FullName::try_from(name).ok()?, badge))
                            })
                            .collect();
                        this.set_badges(map, cx);
                    }
                    Err(failure) => tracing::debug!("forge badges: {}", failure.message),
                }
                if std::mem::take(&mut this.recompute_dirty) {
                    this.start_recompute(cx);
                }
            },
        );
    }

    /// The branches to look pull requests up for: every local branch (by its upstream, else by the push remote and
    /// its own name) and every remote-tracking branch.
    fn queries(&self, cx: &App) -> Vec<BranchQuery> {
        let model = self.ctx.model.read(cx);
        let Some(refs) = &model.refs.value else {
            return Vec::new();
        };
        let push_remote = self.handle.as_ref().and_then(|h| {
            h.facts()
                .push_remote
                .clone()
                .or_else(|| h.facts().base_remote.clone())
        });
        let text = |name: &FullName| name.as_bstr().to_str_lossy().into_owned();
        let mut queries = Vec::new();
        for branch in &refs.local {
            let remote_key = branch
                .upstream
                .as_ref()
                .and_then(|upstream| upstream.tracking.as_ref())
                .map(|tracking| tracking.shorten().to_str_lossy().into_owned())
                .or_else(|| push_remote.as_ref().map(|r| format!("{r}/{}", branch.name)));
            if let Some(remote_key) = remote_key {
                queries.push(BranchQuery {
                    local: text(&branch.full),
                    remote_key,
                    tip: branch.target.to_hex().to_string(),
                });
            }
        }
        for remote in &refs.remotes {
            for branch in &remote.branches {
                queries.push(BranchQuery {
                    local: text(&branch.full),
                    remote_key: format!("{}/{}", branch.remote, branch.name),
                    tip: branch.target.to_hex().to_string(),
                });
            }
        }
        queries
    }

    // ---- events --------------------------------------------------------------------------------------------

    fn on_model(&mut self, event: RepoModelEvent, cx: &mut Context<Self>) {
        let set = event.0;
        if set.intersects(crate::backend::RefreshSet::CONFIG | crate::backend::RefreshSet::REMOTES)
        {
            // A remote may have changed: detect again (no network); a changed forge starts a refresh.
            self.try_detect(true, cx);
        }
        if set.intersects(REFS) {
            self.start_recompute(cx);
        }
    }

    fn on_tab_event(&mut self, event: &RepoTabEvent, cx: &mut Context<Self>) {
        if let RepoTabEvent::OpFinished {
            kind: OpKind::Push,
            ok: true,
        } = event
        {
            // A pushed branch may have a pull request now (or a new head commit with new checks).
            self.request_refresh(cx);
        }
    }

    fn on_accounts_changed(&mut self, cx: &mut Context<Self>) {
        self.poller.user_acted();
        self.detect_attempts = 0;
        self.refresh_after_detect = true;
        self.try_detect(true, cx);
    }
}

fn status_of(facts: &RepoFacts) -> ForgeStatus {
    match (&facts.forge, &facts.account) {
        (None, _) => ForgeStatus::NoForge,
        (Some(forge), _) if !forge.has_api() => ForgeStatus::NoForge,
        (Some(_), AccountMatch::Matching(_)) => ForgeStatus::Idle,
        (Some(_), _) => ForgeStatus::NoAccount,
    }
}

/// The forge preferences of the repository from `repos.json`.
fn repo_prefs(repo: &crate::backend::Repo, cx: &App) -> RepoPrefsInput {
    let path = repo.workdir().unwrap_or_else(|| repo.git_dir());
    let pref = cx
        .try_global::<crate::app::AppState>()
        .map(|state| state.repo_prefs.get(path))
        .unwrap_or_default();
    RepoPrefsInput {
        forge_override: pref.forge_override,
        preferred_user: pref.preferred_forge_user,
        template_path: pref.pr_template_path,
    }
}

/// Registers the forge menu extenders and the accounts model (X4).
pub fn register(cx: &mut App) {
    accounts::install(cx);
    menus::register(cx);
    #[cfg(feature = "qa")]
    qa::register(cx);
}
