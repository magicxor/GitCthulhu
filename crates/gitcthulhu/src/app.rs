//! Application startup (spec 21 §2.2) and the `AppState` global.
//!
//! Order: logging, directories, settings/state/repo prefs → `gpui_kit::init`, fonts, theme, actions (which ends with
//! the region registry) → `AppState` with the Git runtime starting in the background (and the services, feature
//! `services`) → the main window with an OS-native title bar → `AppView` restores the tabs.

use std::path::PathBuf;

use gpui_kit::{
    App, AppContext as _, Bounds, DisplayId, Global, Pixels, TitlebarOptions, WindowBounds,
    WindowOptions, point, px, size,
};

use crate::app_view::AppView;
use crate::assets::AppAssets;
use crate::backend::handle::UndoSettings;
use crate::backend::prompt::UiRequestSender;
use crate::backend::services::ServicesHandle;
use crate::backend::{GitRuntime, GitStatus};
use crate::notifications::ToastKind;
use crate::settings::{self, PlacementKind, RepoPrefs, Settings, UiState, WindowState};
use crate::{actions, backend, fonts, logging, metrics, paths, theme};

/// Process-wide application state.
pub struct AppState {
    /// The Git runtime (`Starting` until `git version` ran).
    pub git: GitStatus,
    /// Forge, AI and update services; `None` until started, or when they are unavailable.
    pub services: Option<ServicesHandle>,
    /// Questions from background threads to the window's request pump.
    pub requests: UiRequestSender,
    /// `repos.json`: per-repository preferences.
    pub repo_prefs: RepoPrefs,
}

impl Global for AppState {}

/// Mutable access to `AppState`, if installed.
pub fn state_mut(cx: &mut App) -> Option<&mut AppState> {
    if cx.has_global::<AppState>() {
        Some(cx.global_mut::<AppState>())
    } else {
        None
    }
}

/// The Git runtime, when it is ready.
pub fn git_runtime(cx: &App) -> Option<std::sync::Arc<GitRuntime>> {
    cx.try_global::<AppState>()
        .and_then(|s| s.git.runtime().cloned())
}

/// The state of the Git runtime (`Starting` before `AppState` exists).
pub fn git_status(cx: &App) -> GitStatus {
    cx.try_global::<AppState>()
        .map_or(GitStatus::Starting, |s| s.git.clone())
}

/// The sender of the window's request pump.
pub fn ui_requests(cx: &App) -> Option<UiRequestSender> {
    cx.try_global::<AppState>().map(|s| s.requests.clone())
}

/// The running services, if any.
pub fn services(cx: &App) -> Option<ServicesHandle> {
    cx.try_global::<AppState>().and_then(|s| s.services.clone())
}

/// The undo settings of the current settings.
pub fn undo_settings(cx: &App) -> UndoSettings {
    UndoSettings::from_settings(&settings::get_or_default(cx))
}

/// Changes `repos.json` and schedules a debounced save.
pub fn update_repo_prefs(cx: &mut App, edit: impl FnOnce(&mut RepoPrefs)) {
    let Some(state) = crate::app::state_mut(cx) else {
        return;
    };
    edit(&mut state.repo_prefs);
    let snapshot = state.repo_prefs.clone();
    settings::schedule_save(cx, "repos", paths::repos_file(), snapshot);
}

/// Installs `AppState` with `GitStatus::Starting`; returns the receiver of the request pump.
pub fn install_state(
    cx: &mut App,
    repo_prefs: RepoPrefs,
) -> futures::channel::mpsc::UnboundedReceiver<crate::backend::prompt::UiRequest> {
    let (requests, receiver) = UiRequestSender::channel();
    settings::set_base(cx, "repos", &repo_prefs);
    cx.set_global(AppState {
        git: GitStatus::Starting,
        services: None,
        requests,
        repo_prefs,
    });
    receiver
}

/// Starts the Git runtime in the background (`git version`); `AppState::git` becomes `Ready` or `Failed`.
pub fn start_git_runtime(cx: &mut App, settings: Settings) {
    let task = backend::bg(cx, move || backend::start_runtime(&settings));
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            let status = match result {
                Ok(rt) => GitStatus::Ready(rt),
                Err(err) => {
                    tracing::error!("git is unavailable: {err}");
                    // 21 §2.9: besides the welcome banner, an error dialog that leads to the Git executable setting.
                    let mut data = crate::backend::present::ErrorDialogData::new(
                        "Git is not available",
                        err.to_string(),
                    );
                    data.extra = Some(crate::backend::present::open_settings_action());
                    crate::app_view::show_error(cx, data);
                    GitStatus::Failed(std::sync::Arc::new(err))
                }
            };
            if let Some(state) = crate::app::state_mut(cx) {
                state.git = status;
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Starts the services in the background; a failure is logged and shown as a Warning toast.
fn start_services(cx: &mut App, settings: Settings) {
    // QA runs never touch the network: no forge, AI or update service.
    #[cfg(feature = "qa")]
    if crate::qa::active() {
        tracing::info!("QA mode: services are not started");
        return;
    }
    let task = backend::bg(cx, move || backend::services::start_services(&settings));
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(handle) => {
                if let Some(state) = crate::app::state_mut(cx) {
                    state.services = Some(handle);
                }
                cx.refresh_windows();
            }
            Err(err) => {
                tracing::warn!("integrations are unavailable: {err:#}");
                if cfg!(feature = "services") {
                    crate::app_view::show_toast(
                        cx,
                        None,
                        ToastKind::Warning,
                        format!("Integrations are unavailable: {err}"),
                        None,
                    );
                }
            }
        });
    })
    .detach();
}

/// The window bounds of a saved placement; the never-saved default is a maximized window whose restore box is a
/// centred 1280×720 box.
pub fn window_bounds(placement: &WindowState, cx: &App) -> WindowBounds {
    let restore = if !placement.is_saved() {
        Bounds::centered(
            None,
            size(
                px(placement.w.max(metrics::WINDOW_DEFAULT_W / 2.)),
                px(placement.h.max(metrics::WINDOW_DEFAULT_H / 2.)),
            ),
            cx,
        )
    } else {
        Bounds {
            origin: point(px(placement.x), px(placement.y)),
            size: size(px(placement.w), px(placement.h)),
        }
    };
    match placement.placement {
        PlacementKind::Floating => WindowBounds::Windowed(restore),
        PlacementKind::Maximized => WindowBounds::Maximized(restore),
        PlacementKind::Fullscreen => WindowBounds::Fullscreen(restore),
    }
}

/// The saved form of a window's bounds on the display with UUID `display`.
pub fn window_state(bounds: WindowBounds, display: Option<String>) -> WindowState {
    let (placement, b) = match bounds {
        WindowBounds::Windowed(b) => (PlacementKind::Floating, b),
        WindowBounds::Maximized(b) => (PlacementKind::Maximized, b),
        WindowBounds::Fullscreen(b) => (PlacementKind::Fullscreen, b),
    };
    WindowState {
        placement,
        x: b.origin.x.as_f32(),
        y: b.origin.y.as_f32(),
        w: b.size.width.as_f32(),
        h: b.size.height.as_f32(),
        saved: true,
        display,
    }
}

/// The display a saved placement belongs on, among `displays` (id, UUID, bounds): the one with the saved UUID, else
/// the one holding the centre of the restore box. `None` (the primary display) for a never-saved placement or when
/// none matches, e.g. the display was unplugged.
pub fn placement_display(
    placement: &WindowState,
    displays: impl IntoIterator<Item = (DisplayId, Option<String>, Bounds<Pixels>)>,
) -> Option<DisplayId> {
    if !placement.is_saved() {
        return None;
    }
    let centre = point(
        px(placement.x + placement.w / 2.),
        px(placement.y + placement.h / 2.),
    );
    let mut holding_centre = None;
    for (id, uuid, bounds) in displays {
        if uuid.is_some() && uuid == placement.display {
            return Some(id);
        }
        if holding_centre.is_none() && bounds.contains(&centre) {
            holding_centre = Some(id);
        }
    }
    holding_centre
}

/// The display of `placement` among the current displays (see [`placement_display`]).
fn saved_display(placement: &WindowState, cx: &App) -> Option<DisplayId> {
    let displays = cx.displays().into_iter().map(|display| {
        (
            display.id(),
            display.uuid().ok().map(|uuid| uuid.to_string()),
            display.bounds(),
        )
    });
    placement_display(placement, displays)
}

/// The repository directory of the first command-line argument (`/.git` stripped), if any.
pub fn cli_repository() -> Option<PathBuf> {
    let arg = std::env::args_os().nth(1)?;
    Some(strip_git_dir(PathBuf::from(arg)))
}

/// `repo/.git` → `repo`.
pub fn strip_git_dir(path: PathBuf) -> PathBuf {
    if path.file_name().is_some_and(|n| n == ".git")
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        return parent.to_path_buf();
    }
    path
}

/// What [`load_settings`] read: the three files, what the user is told about those that did not load as they were
/// (a reset file or keys), and per save name the values that were reset (see `settings::io::note_rejected`).
struct LoadedSettings {
    settings: Settings,
    ui_state: UiState,
    repo_prefs: RepoPrefs,
    notices: Vec<String>,
    rejected: Vec<(&'static str, serde_json::Map<String, serde_json::Value>)>,
}

fn load_settings() -> LoadedSettings {
    let settings = Settings::load_from(&paths::settings_file());
    let ui_state = UiState::load_from(&paths::state_file());
    let repo_prefs = RepoPrefs::load_from(&paths::repos_file());
    let notices = [
        settings.notice("settings.json"),
        ui_state.notice("state.json"),
        repo_prefs.notice("repos.json"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let rejected = vec![
        ("settings", settings.rejected()),
        ("state", ui_state.rejected()),
        ("repos", repo_prefs.rejected()),
    ];
    LoadedSettings {
        settings: settings.into_value(),
        ui_state: ui_state.into_value(),
        repo_prefs: repo_prefs.into_value(),
        notices,
        rejected,
    }
}

/// Writes `repos.json` now (on quit) if this process changed it, so a change still in its debounce is kept.
fn flush_repo_prefs(cx: &App) {
    if let Some(state) = cx.try_global::<AppState>() {
        settings::io::flush_now(cx, "repos", &paths::repos_file(), &state.repo_prefs);
    }
}

/// Shows what [`load_settings`] has to tell as Warning toasts (once the window's toast host exists).
fn show_load_notices(cx: &mut App, notices: Vec<String>) {
    for notice in notices {
        crate::app_view::show_toast(cx, None, ToastKind::Warning, notice, None);
    }
}

/// Starts the application and blocks until the last window is closed.
pub fn run() {
    // A qa build refuses to start outside a QA sandbox, before anything is read.
    #[cfg(feature = "qa")]
    let qa = crate::qa::enter();
    logging::init();
    if let Err(err) = paths::ensure_dirs() {
        tracing::warn!("cannot create the application directories: {err}");
    }
    let LoadedSettings {
        settings,
        ui_state,
        repo_prefs,
        notices,
        rejected,
    } = load_settings();

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            #[cfg(feature = "qa")]
            crate::qa::init(cx, &qa);
            fonts::register(cx);
            settings::init(cx, settings.clone());
            settings::state::init(cx, ui_state.clone());
            theme::init(cx, &settings);
            theme::observe(cx);
            actions::init(cx);

            let requests = install_state(cx, repo_prefs);
            for (name, values) in rejected.clone() {
                settings::io::note_rejected(cx, name, values);
            }
            start_git_runtime(cx, settings.clone());
            start_services(cx, settings.clone());

            #[allow(unused_mut)]
            let mut options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("GitCthulhu".into()),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds(&ui_state.window, cx)),
                display_id: saved_display(&ui_state.window, cx),
                app_id: Some("gitcthulhu".into()),
                ..Default::default()
            };
            // QA runs open a fixed 1280x720 window and never take the focus from the user's work.
            #[cfg(feature = "qa")]
            {
                options.window_bounds = Some(crate::qa::window_bounds());
                options.display_id = None;
                options.focus = false;
            }
            // A qa run opens repositories only through its scenario (its arguments are not repository paths).
            #[cfg(feature = "qa")]
            let cli = None;
            #[cfg(not(feature = "qa"))]
            let cli = cli_repository();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| AppView::new(requests, cli, window, cx))
            });
            if let Err(err) = &opened {
                tracing::error!("cannot open the main window: {err}");
                cx.quit();
                return;
            }
            show_load_notices(cx, notices.clone());
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    settings::flush(cx);
                    flush_repo_prefs(cx);
                    cx.quit();
                }
            })
            .detach();
            #[cfg(feature = "qa")]
            if let Ok((handle, _)) = &opened {
                crate::qa::attach(&qa, *handle, cx);
            }
            #[cfg(not(feature = "qa"))]
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn display(id: u64, uuid: &str, x: f32) -> (DisplayId, Option<String>, Bounds<Pixels>) {
        (
            DisplayId::new(id),
            Some(uuid.to_owned()),
            Bounds {
                origin: point(px(x), px(0.)),
                size: size(px(1920.), px(1080.)),
            },
        )
    }

    #[test]
    fn a_saved_window_reopens_on_its_display() {
        let on_second = WindowState {
            placement: PlacementKind::Maximized,
            x: 2100.,
            y: 100.,
            w: 1280.,
            h: 720.,
            saved: true,
            display: Some("second".into()),
        };
        let displays = || [display(1, "primary", 0.), display(2, "second", 1920.)];
        assert_eq!(
            placement_display(&on_second, displays()),
            Some(DisplayId::new(2)),
            "the display with the saved UUID"
        );
        let renamed = WindowState {
            display: Some("gone".into()),
            ..on_second
        };
        assert_eq!(
            placement_display(&renamed, displays()),
            Some(DisplayId::new(2)),
            "else the display holding the centre of the restore box"
        );
        let unplugged = WindowState {
            x: 5000.,
            ..renamed
        };
        assert_eq!(
            placement_display(&unplugged, displays()),
            None,
            "the primary display when none matches"
        );
        assert_eq!(
            placement_display(&WindowState::default(), displays()),
            None,
            "a never-saved window opens on the primary display"
        );
    }

    #[test]
    fn a_window_saved_at_the_origin_is_saved() {
        let at_origin = window_state(
            WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: size(px(900.), px(600.)),
            }),
            None,
        );
        assert!(
            at_origin.is_saved(),
            "a real floating window at (0, 0) is not the default"
        );
        assert!(!WindowState::default().is_saved(), "the default is not");
        let old_file = WindowState {
            x: 10.,
            ..WindowState::default()
        };
        assert!(old_file.is_saved(), "files from before `saved` still count");
    }

    #[test]
    fn a_minimized_window_is_recognised() {
        let at = |x: f32, y: f32| Bounds {
            origin: point(px(x), px(y)),
            size: size(px(160.), px(28.)),
        };
        assert!(
            crate::app_view::minimized(at(-32000., -32000.)),
            "where Windows parks a minimized window"
        );
        assert!(
            !crate::app_view::minimized(at(-1920., 0.)),
            "a display left of the primary one"
        );
    }

    #[test]
    fn strip_git_dir_removes_only_a_trailing_dot_git() {
        assert_eq!(
            strip_git_dir(PathBuf::from("/x/repo/.git")),
            PathBuf::from("/x/repo"),
            "repo/.git opens repo"
        );
        assert_eq!(
            strip_git_dir(PathBuf::from("/x/repo.git")),
            PathBuf::from("/x/repo.git"),
            "a bare repository directory stays"
        );
        assert_eq!(
            strip_git_dir(PathBuf::from(".git")),
            PathBuf::from(".git"),
            "a lone .git has no parent to open"
        );
    }
}
