//! Application startup (spec 21 §2.2) and the `AppState` global.
//!
//! Order: logging, directories, settings/state/repo prefs → `gpui_kit::init`, fonts, theme, actions (which ends with
//! the region registry) → `AppState` with the Git runtime starting in the background (and the services, feature
//! `services`) → the main window with an OS-native title bar → `AppView` restores the tabs.

use std::path::PathBuf;

use gpui_kit::{
    App, AppContext as _, Bounds, Global, TitlebarOptions, WindowBounds, WindowOptions, point, px,
    size,
};

use crate::app_view::AppView;
use crate::assets::AppAssets;
use crate::backend::handle::UndoSettings;
use crate::backend::prompt::UiRequestSender;
use crate::backend::services::ServicesHandle;
use crate::backend::{GitRuntime, GitStatus};
use crate::notifications::ToastKind;
use crate::settings::{self, Loaded, PlacementKind, RepoPrefs, Settings, UiState, WindowState};
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
    let never_saved = placement.x == 0. && placement.y == 0.;
    let restore = if never_saved {
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

/// The saved form of a window's bounds.
pub fn window_state(bounds: WindowBounds) -> WindowState {
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
    }
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

fn load_settings() -> (Settings, UiState, RepoPrefs) {
    let settings = match Settings::load_from(&paths::settings_file()) {
        Loaded::Corrupt { value, backup } => {
            tracing::warn!("settings.json was corrupt; backup: {backup:?}");
            value
        }
        loaded => loaded.into_value(),
    };
    let ui_state = match UiState::load_from(&paths::state_file()) {
        Loaded::Corrupt { value, backup } => {
            tracing::warn!("state.json was corrupt; backup: {backup:?}");
            value
        }
        loaded => loaded.into_value(),
    };
    let repo_prefs = match RepoPrefs::load_from(&paths::repos_file()) {
        Loaded::Corrupt { value, backup } => {
            tracing::warn!("repos.json was corrupt; backup: {backup:?}");
            value
        }
        loaded => loaded.into_value(),
    };
    (settings, ui_state, repo_prefs)
}

/// Starts the application and blocks until the last window is closed.
pub fn run() {
    logging::init();
    if let Err(err) = paths::ensure_dirs() {
        tracing::warn!("cannot create the application directories: {err}");
    }
    let (settings, ui_state, repo_prefs) = load_settings();

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            settings::init(cx, settings.clone());
            settings::state::init(cx, ui_state.clone());
            theme::init(cx, &settings);
            theme::observe(cx);
            actions::init(cx);

            let requests = install_state(cx, repo_prefs);
            start_git_runtime(cx, settings.clone());
            start_services(cx, settings.clone());

            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("GitCthulhu".into()),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds(&ui_state.window, cx)),
                app_id: Some("gitcthulhu".into()),
                ..Default::default()
            };
            let cli = cli_repository();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| AppView::new(requests, cli, window, cx))
            });
            if let Err(err) = opened {
                tracing::error!("cannot open the main window: {err}");
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    settings::flush(cx);
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

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
