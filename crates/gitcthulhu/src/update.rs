//! Update notice and avatars (D0.5; owned by X5 after G0b; spec 21 §5.6, §2.10, §2.13.3). Milestone 1: no notice,
//! no Updates settings page, no avatar fetcher (initials only).

use gpui_kit::{AnyElement, App, Window};

/// The GitCthulhu releases page (the bottom bar's version link).
pub const RELEASES_URL: &str = "https://github.com/gitcthulhu/gitcthulhu/releases";

/// The update notice before the version link of the bottom bar. Milestone 1: `None`.
pub fn bottom_bar_slot(_window: &mut Window, _cx: &mut App) -> Option<AnyElement> {
    None
}

/// Whether the settings dialog shows the Updates page. Milestone 1: `false`.
pub fn settings_available(_cx: &App) -> bool {
    false
}

/// Registers the update UI and installs the avatar fetcher (X5).
pub fn register(_cx: &mut App) {}
