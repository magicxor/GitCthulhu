//! `state.json`: UI state that survives restarts (spec 21 §7.3).

use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

use super::io::{Loaded, load, schedule_save, write_atomic};
use crate::metrics;

/// Maximum number of remembered repositories.
pub const MAX_RECENT: usize = 100;

/// Window placement kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementKind {
    /// A normal window at `x, y, w, h`.
    Floating,
    /// Maximized; `x, y, w, h` is the restore box.
    #[default]
    Maximized,
    /// Full screen; `x, y, w, h` is the restore box.
    Fullscreen,
}

/// Saved window placement, in logical pixels.
///
/// The default (never saved) is "maximized, restore box 1280 x 720 with `x == y == 0`"; the window code centres a
/// box whose origin is `(0, 0)` on the primary display.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub placement: PlacementKind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            placement: PlacementKind::Maximized,
            x: 0.,
            y: 0.,
            w: metrics::WINDOW_DEFAULT_W,
            h: metrics::WINDOW_DEFAULT_H,
        }
    }
}

/// Persisted UI state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Paths of tabs with a repository.
    pub tabs_open: Vec<PathBuf>,
    /// Selected tab; an invalid index is treated as 0 by [`UiState::selected_tab_index`].
    pub selected_tab: usize,
    /// Most recent first, at most [`MAX_RECENT`], de-duplicated by canonical path.
    pub recent_repositories: Vec<PathBuf>,
    /// Width of the left pane in dp (global for all tabs).
    pub first_pane_width: f32,
    /// Width of the right pane in dp (global for all tabs).
    pub third_pane_width: f32,
    /// Height of the terminal panel in dp.
    pub terminal_height: f32,
    pub window: WindowState,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tabs_open: Vec::new(),
            selected_tab: 0,
            recent_repositories: Vec::new(),
            first_pane_width: metrics::SIDE_PANE_W,
            third_pane_width: metrics::RIGHT_PANE_W,
            terminal_height: metrics::TERMINAL_DEFAULT_H,
            window: WindowState::default(),
        }
    }
}

/// Canonical form of a repository path used for de-duplication; falls back to the path itself when it cannot be
/// canonicalised (for example a repository on a disconnected drive).
pub fn canonical(path: &Path) -> PathBuf {
    canonicalize_plain(path).unwrap_or_else(|| path.to_owned())
}

/// `std::fs::canonicalize` without the `\\?\` verbatim prefix Windows adds, so that stored paths stay readable and
/// compare equal to paths typed by the user.
fn canonicalize_plain(path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    #[cfg(windows)]
    {
        let text = canonical.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return Some(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Some(PathBuf::from(rest));
        }
    }
    Some(canonical)
}

impl UiState {
    /// Loads the state from `path`: defaults when missing, defaults plus a `*.bak` rename when corrupt.
    pub fn load_from(path: &Path) -> Loaded<UiState> {
        load::<UiState>(path)
    }

    /// Writes the state to `path` atomically.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, self)
    }

    /// The selected tab, or 0 when the stored index is out of range.
    pub fn selected_tab_index(&self) -> usize {
        if self.selected_tab < self.tabs_open.len() {
            self.selected_tab
        } else {
            0
        }
    }

    /// Records `path` as the most recently opened repository: moves an existing entry (compared by canonical path)
    /// to the front and truncates to [`MAX_RECENT`].
    ///
    /// Only `path` is canonicalised: stored entries are canonical already (this is the only writer), and
    /// canonicalising up to 100 entries on the UI thread would block on unreachable network drives.
    pub fn push_recent(&mut self, path: &Path) {
        let canon = canonical(path);
        self.recent_repositories
            .retain(|p| *p != canon && p != path);
        self.recent_repositories.insert(0, canon);
        self.recent_repositories.truncate(MAX_RECENT);
    }

    /// Removes `path` (compared by canonical path) from the recent repositories.
    pub fn remove_recent(&mut self, path: &Path) {
        let canon = canonical(path);
        self.recent_repositories
            .retain(|p| *p != canon && p != path);
    }
}

/// The UI state global.
pub struct UiStateGlobal(pub UiState);

impl Global for UiStateGlobal {}

/// Installs `state` as the global (without scheduling a save).
pub fn init(cx: &mut App, state: UiState) {
    cx.set_global(UiStateGlobal(state));
}

/// The current UI state. Panics if [`init`] has not run (a programming error).
pub fn get(cx: &App) -> &UiState {
    &cx.global::<UiStateGlobal>().0
}

/// Changes the UI state, notifies `observe_global::<UiStateGlobal>` observers and schedules a debounced save.
pub fn update(cx: &mut App, edit: impl FnOnce(&mut UiState)) {
    let mut next = cx
        .try_global::<UiStateGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default();
    edit(&mut next);
    cx.set_global(UiStateGlobal(next.clone()));
    schedule_save(cx, "state", crate::paths::state_file(), next);
}

/// Writes the state immediately (on quit).
pub fn flush(cx: &App) {
    if let Some(s) = cx.try_global::<UiStateGlobal>()
        && let Err(err) = s.0.save_to(&crate::paths::state_file())
    {
        tracing::error!("cannot save UI state: {err}");
    }
}
