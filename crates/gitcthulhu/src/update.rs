//! Update notice and avatars (D0.5; owned by X5; spec 21 §5.6, §5.4, §2.10).
//!
//! * **Update check.** When the running services were started with a manifest URL
//!   ([`crate::backend::services::ServicesHandle::updates_enabled`]; there is no default URL, so a build without
//!   `GITCTHULHU_UPDATE_MANIFEST_URL` never fetches anything), one app-level task looks for a new version at startup
//!   and then every [`CHECK_INTERVAL`] while "Check for updates automatically" is on. The outcome lives in the
//!   [`UpdateState`] global: the bottom bar's notice ([`bottom_bar_slot`]), the Updates settings page
//!   (`dialogs::settings::updates`) and the toasts of a manual check ([`check_now`]) read it.
//! * **Avatars.** Once the services run, [`install_avatar_fetcher`] gives `widgets::avatar` a downloader: Gravatar
//!   images come through the forge HTTP stack, are cached on disk ([`avatars`]) and fall back to the initial letter
//!   when offline.
//!
//! All service calls block and run inside `backend::bg`; the UI thread only reads and writes the state.

use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::{AnyElement, App, AsyncApp, Global, SharedString, Task, Window};

use crate::backend::update::{ImageFetch, UpdateCheckError, UpdateNotice};
use crate::backend::{self, CancelToken};
use crate::notifications::{ToastAction, ToastKind};
use crate::settings::{self, SettingsGlobal};
use crate::{app, app_view};

pub mod avatars;
#[cfg(any(test, feature = "qa"))]
pub mod loopback;
mod notice;
#[cfg(feature = "qa")]
mod qa;
#[cfg(test)]
mod tests;

/// The GitCthulhu releases page (the bottom bar's version link).
pub const RELEASES_URL: &str = "https://github.com/gitcthulhu/gitcthulhu/releases";

/// How often the automatic check runs (spec 21 §2.10: every 24 h).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// How often the scheduler looks whether a check is due (so that switching the setting on, or a laptop waking up,
/// is noticed without a restart).
pub const SCHEDULER_TICK: Duration = Duration::from_secs(15 * 60);

/// How long after a failed avatar download (offline) the failed avatars are asked for again.
pub const AVATAR_RETRY: Duration = Duration::from_secs(10 * 60);

/// Snooze of "Remind me tomorrow".
pub const SNOOZE_DAY_HOURS: u32 = 24;
/// Snooze of "Remind me in a week".
pub const SNOOZE_WEEK_HOURS: u32 = 168;

/// Element ids of the update notice (tests, QA).
pub mod ids {
    /// The "Update {version} available" button of the bottom bar.
    pub const NOTICE: &str = "update-notice";
    /// The context-menu entry "Remind me tomorrow".
    pub const REMIND_TOMORROW: &str = "update-remind-tomorrow";
    /// The context-menu entry "Remind me in a week".
    pub const REMIND_WEEK: &str = "update-remind-week";
}

/// Whether a check was asked for by the user (it reports its outcome in a toast) or ran by itself (silent unless
/// there is something to show).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    /// "Check now" in the settings.
    Manual,
    /// Startup and the 24 h schedule.
    Automatic,
}

/// What the app knows about updates (an app global; created on first use).
#[derive(Default)]
pub struct UpdateState {
    notice: Option<UpdateNotice>,
    checking: bool,
    /// Snoozes whose service call has not finished.
    pending_snoozes: u32,
    last_checked: Option<SystemTime>,
    last_error: Option<SharedString>,
    scheduler_started: bool,
    /// The "Check for updates automatically" value the settings observer saw last (it checks only when the setting
    /// is switched on, not on every unrelated settings change).
    auto_check_seen: Option<bool>,
    /// "Check now" was asked for while an automatic check was running: that check reports in a toast.
    manual_waiting: bool,
    /// Snoozes that finished; a check that started before one of them must not bring the notice back.
    snoozes_done: u64,
    /// The installed avatar downloader (re-installed after a network failure, see [`schedule_avatar_retry`]).
    fetcher: Option<crate::widgets::avatar::AvatarFetcher>,
    /// A re-install of the avatar downloader is scheduled (until then downloads answer from the disk cache only).
    avatar_retry_pending: bool,
    /// Limits the avatar downloads running at once.
    avatar_gate: avatars::DownloadGate,
}

impl Global for UpdateState {}

/// A copy of the state for rendering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateSnapshot {
    /// The update to announce, if any.
    pub notice: Option<UpdateNotice>,
    /// A check is running.
    pub checking: bool,
    /// When the last check finished successfully (this session).
    pub last_checked: Option<SystemTime>,
    /// Why the last check failed, if it did.
    pub last_error: Option<SharedString>,
}

/// The current state.
pub fn snapshot(cx: &App) -> UpdateSnapshot {
    cx.try_global::<UpdateState>()
        .map(|s| UpdateSnapshot {
            notice: s.notice.clone(),
            checking: s.checking,
            last_checked: s.last_checked,
            last_error: s.last_error.clone(),
        })
        .unwrap_or_default()
}

/// The update to announce, if any.
pub fn notice(cx: &App) -> Option<UpdateNotice> {
    cx.try_global::<UpdateState>()
        .and_then(|s| s.notice.clone())
}

/// Whether a check is running.
pub fn is_checking(cx: &App) -> bool {
    cx.try_global::<UpdateState>().is_some_and(|s| s.checking)
}

/// The update notice before the version link of the bottom bar (07 §7): `None` while there is no update.
pub fn bottom_bar_slot(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    notice::slot(window, cx)
}

/// Whether the settings dialog shows the Updates page: the services run and this build has a manifest URL.
pub fn settings_available(cx: &App) -> bool {
    enabled_services(cx).is_some()
}

/// The services, only when update checks are possible (a manifest URL is configured).
fn enabled_services(cx: &App) -> Option<backend::services::ServicesHandle> {
    app::services(cx).filter(updates_enabled)
}

#[cfg(feature = "services")]
fn updates_enabled(services: &backend::services::ServicesHandle) -> bool {
    services.updates_enabled
}

#[cfg(not(feature = "services"))]
fn updates_enabled(_services: &backend::services::ServicesHandle) -> bool {
    false
}

/// Whether an automatic check is due: the setting is on and the last check is older than [`CHECK_INTERVAL`] (or
/// there was none).
pub fn check_due(enabled: bool, last_checked: Option<SystemTime>, now: SystemTime) -> bool {
    enabled
        && last_checked
            .is_none_or(|last| now.duration_since(last).unwrap_or(Duration::ZERO) >= CHECK_INTERVAL)
}

/// Starts a check unless one is running. `Manual` checks report in a toast; an `Automatic` one only logs a failure.
pub fn check_now(cx: &mut App, kind: CheckKind) {
    let Some(services) = enabled_services(cx) else {
        if kind == CheckKind::Manual {
            app_view::show_toast(
                cx,
                None,
                ToastKind::Warning,
                "This build of GitCthulhu has no update source configured.",
                None,
            );
        }
        return;
    };
    let snoozes_before = {
        let state = cx.default_global::<UpdateState>();
        if state.checking {
            // A check is already running: its outcome answers this request too (in a toast when asked for).
            state.manual_waiting |= kind == CheckKind::Manual;
            return;
        }
        state.checking = true;
        state.manual_waiting = false;
        state.snoozes_done
    };
    cx.refresh_windows();
    let cancel = CancelToken::new();
    let task = backend::bg(cx, move || backend::update::check_now(&services, &cancel));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = task.await;
        cx.update(|cx| finish_check(cx, kind, snoozes_before, result));
    })
    .detach();
}

fn finish_check(
    cx: &mut App,
    kind: CheckKind,
    snoozes_before: u64,
    result: Result<Option<UpdateNotice>, UpdateCheckError>,
) {
    let state = cx.default_global::<UpdateState>();
    state.checking = false;
    let kind = if std::mem::take(&mut state.manual_waiting) {
        CheckKind::Manual
    } else {
        kind
    };
    match &result {
        Ok(found) => {
            // A snooze that finished while this check was running wins over the check's answer (which may have
            // been read from the cache just before the snooze was written).
            let snoozed_meanwhile = state.snoozes_done != snoozes_before;
            state.notice = if snoozed_meanwhile {
                None
            } else {
                found.clone()
            };
            state.last_checked = Some(SystemTime::now());
            state.last_error = None;
        }
        Err(UpdateCheckError::Failed(reason)) => state.last_error = Some(reason.clone().into()),
        Err(UpdateCheckError::Disabled | UpdateCheckError::Cancelled) => {}
    }
    cx.refresh_windows();
    match (result, kind) {
        (Ok(Some(found)), CheckKind::Manual) => {
            let url = found.url.clone();
            app_view::show_toast(
                cx,
                None,
                ToastKind::Positive,
                format!("GitCthulhu {} is available.", found.version),
                Some(ToastAction {
                    label: "Download".into(),
                    run: Rc::new(move |_, cx| notice::open_download(cx, &url)),
                }),
            );
        }
        (Ok(None), CheckKind::Manual) => app_view::show_toast(
            cx,
            None,
            ToastKind::Positive,
            "GitCthulhu is up to date.",
            None,
        ),
        (Err(UpdateCheckError::Failed(reason)), CheckKind::Manual) => app_view::show_toast(
            cx,
            None,
            ToastKind::Error,
            format!("Checking for updates failed: {reason}"),
            None,
        ),
        (Err(UpdateCheckError::Failed(reason)), CheckKind::Automatic) => {
            tracing::warn!("the update check failed: {reason}");
        }
        _ => {}
    }
}

/// Hides the notice for `hours` (the "Remind me" entries of its context menu) and says so in a toast.
pub fn snooze(cx: &mut App, hours: u32) {
    let Some(services) = enabled_services(cx) else {
        return;
    };
    cx.default_global::<UpdateState>().pending_snoozes += 1;
    let task = backend::bg(cx, move || backend::update::snooze(&services, hours));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(()) => {
                let state = cx.default_global::<UpdateState>();
                state.pending_snoozes = state.pending_snoozes.saturating_sub(1);
                state.snoozes_done += 1;
                state.notice = None;
                cx.refresh_windows();
                app_view::show_toast(cx, None, ToastKind::Positive, snooze_text(hours), None);
            }
            Err(err) => {
                let state = cx.default_global::<UpdateState>();
                state.pending_snoozes = state.pending_snoozes.saturating_sub(1);
                app_view::show_toast(
                    cx,
                    None,
                    ToastKind::Error,
                    format!("Cannot postpone the update reminder: {err}"),
                    None,
                );
            }
        });
    })
    .detach();
}

/// The toast after a snooze.
pub fn snooze_text(hours: u32) -> String {
    match hours {
        SNOOZE_DAY_HOURS => "We will remind you about this update tomorrow.".to_owned(),
        SNOOZE_WEEK_HOURS => "We will remind you about this update in a week.".to_owned(),
        other => format!("We will remind you about this update in {other} hours."),
    }
}

/// Runs one scheduler step: a check when one is due.
fn tick(cx: &mut App) {
    let enabled = settings::get_or_default(cx).update_check;
    let last = cx.try_global::<UpdateState>().and_then(|s| s.last_checked);
    if check_due(enabled, last, SystemTime::now()) {
        check_now(cx, CheckKind::Automatic);
    }
}

/// Starts the app-level update task once the services run with a manifest URL: shows the cached notice at once,
/// then checks at startup and every [`CHECK_INTERVAL`].
fn ensure_scheduler(cx: &mut App) {
    let Some(services) = enabled_services(cx) else {
        return;
    };
    let auto_check = settings::get_or_default(cx).update_check;
    {
        let state = cx.default_global::<UpdateState>();
        if state.scheduler_started {
            return;
        }
        state.scheduler_started = true;
        state.auto_check_seen = Some(auto_check);
    }
    let cached = backend::bg(cx, move || backend::update::cached(&services));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let cached = cached.await;
        cx.update(|cx| {
            let state = cx.default_global::<UpdateState>();
            // A check that already finished knows better than the cache.
            if state.last_checked.is_none() && state.notice.is_none() {
                state.notice = cached;
                cx.refresh_windows();
            }
        });
        loop {
            cx.update(tick);
            cx.background_executor().timer(SCHEDULER_TICK).await;
        }
    })
    .detach();
}

/// Gives `widgets::avatar` its downloader once the services run (before that the cache has no fetcher and the
/// initials stay; installing it retries the images that were missing).
fn install_avatar_fetcher(cx: &mut App) {
    // QA runs never leave the machine: `update serve` installs services, but avatars keep their initials.
    #[cfg(feature = "qa")]
    if crate::qa::active() {
        return;
    }
    let Some(services) = app::services(cx) else {
        return;
    };
    if cx.default_global::<UpdateState>().fetcher.is_some() {
        return;
    }
    let cache = avatars::AvatarDiskCache::new(crate::paths::cache_dir().join("avatars"));
    let pruned = cache.clone();
    backend::bg(cx, move || pruned.prune(avatars::MAX_FILES)).detach();
    let fetcher = move |url: String, cx: &mut App| -> Task<anyhow::Result<Vec<u8>>> {
        // Unit tests never ask gravatar.com: without a loopback stand-in the download fails (initials).
        #[cfg(test)]
        let Some(url) = tests::redirect_avatar_url(cx, url) else {
            return Task::ready(Err(anyhow::anyhow!("unit tests do not download avatars")));
        };
        let services = services.clone();
        let cache = cache.clone();
        let turn = cx.default_global::<UpdateState>().avatar_gate.enter();
        cx.spawn(async move |cx: &mut AsyncApp| {
            if let Some(turn) = turn {
                // `Canceled` only when the gate itself is gone: go ahead then.
                let _ = turn.await;
            }
            // A failed download already scheduled a retry: until then the network is known to be down, so the
            // queued avatars are answered from the disk cache alone instead of each waiting for the timeout.
            let offline = cx.update(|cx| cx.default_global::<UpdateState>().avatar_retry_pending);
            let task = cx.update(|cx| {
                backend::bg(cx, move || {
                    avatars::load(&cache, &url, SystemTime::now(), |url| {
                        if offline {
                            ImageFetch::Failed("offline; asked again later".to_owned())
                        } else {
                            backend::update::fetch_image(&services, url)
                        }
                    })
                })
            });
            let result = task.await;
            cx.update(|cx| {
                cx.default_global::<UpdateState>().avatar_gate.leave();
                if matches!(result, Err(avatars::AvatarMiss::Unreachable(_))) {
                    schedule_avatar_retry(cx);
                }
            });
            result.map_err(|miss| anyhow::Error::msg(miss.to_string()))
        })
    };
    let fetcher: crate::widgets::avatar::AvatarFetcher = Rc::new(fetcher);
    cx.default_global::<UpdateState>().fetcher = Some(fetcher.clone());
    crate::widgets::avatar::set_fetcher(cx, fetcher);
}

/// After a download failed (offline, server error) the avatar widget shows the initial letter for the rest of the
/// session; re-installing the downloader after [`AVATAR_RETRY`] makes it ask again (addresses known to have no
/// picture are answered by the disk cache, so only the failed ones reach the network). While the retry is pending
/// the downloader does not use the network at all.
fn schedule_avatar_retry(cx: &mut App) {
    let state = cx.default_global::<UpdateState>();
    if state.avatar_retry_pending {
        return;
    }
    state.avatar_retry_pending = true;
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(AVATAR_RETRY).await;
        cx.update(|cx| {
            let state = cx.default_global::<UpdateState>();
            state.avatar_retry_pending = false;
            if let Some(fetcher) = state.fetcher.clone() {
                crate::widgets::avatar::set_fetcher(cx, fetcher);
            }
        });
    })
    .detach();
}

/// Reacts to the services having started (or the settings having changed).
fn on_services_changed(cx: &mut App) {
    install_avatar_fetcher(cx);
    ensure_scheduler(cx);
}

/// Registers the update UI: reacts to the services starting (avatar fetcher, update task) and to the "Check for
/// updates automatically" setting, and adds the QA verbs.
pub fn register(cx: &mut App) {
    cx.observe_global::<app::AppState>(on_services_changed)
        .detach();
    cx.observe_global::<SettingsGlobal>(|cx| {
        let auto_check = settings::get_or_default(cx).update_check;
        let Some(state) = cx.try_global::<UpdateState>() else {
            return;
        };
        if !state.scheduler_started {
            return;
        }
        let switched_on = auto_check && state.auto_check_seen == Some(false);
        cx.default_global::<UpdateState>().auto_check_seen = Some(auto_check);
        // Only switching the setting on checks at once; other settings changes (typing in a field of the dialog)
        // must not start a check each, even while the last one failed.
        if switched_on {
            tick(cx);
        }
    })
    .detach();
    #[cfg(feature = "qa")]
    qa::register(cx);
}
