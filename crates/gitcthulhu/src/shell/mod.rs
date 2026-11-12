//! The application shell (owned by G1 after G0b; spec 21 §1.1, 07 §2-§4, §7): tab strip, welcome page, recent
//! repositories, loading screen and processing overlay, bottom bar. G0b ships placeholders at their final bounds.

use gpui_kit::App;

pub mod bottom_bar;
pub mod loading;
pub mod recent_repos;
pub mod tab_strip;
pub mod welcome;

/// Element ids of the shell (the frame ids are in `crate::ids`).
pub mod ids {
    /// The welcome page.
    pub const WELCOME: &str = "welcome";
    /// The welcome page's Open tile.
    pub const WELCOME_OPEN: &str = "welcome-open";
    /// The recent repositories list.
    pub const RECENT_REPOS: &str = "recent-repos";
    /// The "Opening repository" screen.
    pub const LOADING: &str = "loading-repository";
    /// The processing overlay's content.
    pub const PROCESSING: &str = "processing";
    /// The processing overlay's Cancel button.
    pub const PROCESSING_CANCEL: &str = "processing-cancel";
}

/// Registers the shell's key bindings and actions (G1).
pub fn register(_cx: &mut App) {}
