//! The application shell (owned by G1; spec 21 §1.1, 07 §2-§4, §7): tab strip, welcome page, recent repositories,
//! loading screen and processing overlay, bottom bar.
//!
//! The shell has no `metrics::<region>` file: its sizes that no other region shares live in [`dims`]; the shared ones
//! (tab strip, bottom bar, ...) are in `crate::metrics`.

use gpui_kit::{App, KeyBinding};

pub mod bottom_bar;
pub mod loading;
pub mod platform;
pub mod recent_repos;
pub mod tab_strip;
pub mod welcome;

#[cfg(test)]
mod tests;

/// Element ids of the shell (the frame ids are in `crate::ids`).
pub mod ids {
    /// The welcome page.
    pub const WELCOME: &str = "welcome";
    /// The welcome page's content group (title, buttons and recent list).
    pub const WELCOME_GROUP: &str = "welcome-group";
    /// The welcome page's title.
    pub const WELCOME_TITLE: &str = "welcome-title";
    /// The banner shown when Git is unavailable.
    pub const WELCOME_GIT_BANNER: &str = "welcome-git-banner";
    /// The welcome page's Open tile.
    pub const WELCOME_OPEN: &str = "welcome-open";
    /// The welcome page's Clone tile.
    pub const WELCOME_CLONE: &str = "welcome-clone";
    /// The welcome page's "Start a local repository" tile.
    pub const WELCOME_INIT: &str = "welcome-init";
    /// The "Additional information" heading.
    pub const WELCOME_MORE: &str = "welcome-more";
    /// The "Source code" button.
    pub const WELCOME_SOURCE: &str = "welcome-source";
    /// The "Report a bug" button.
    pub const WELCOME_BUG: &str = "welcome-bug";
    /// The "Additional information" button (About dialog).
    pub const WELCOME_ABOUT: &str = "welcome-about";
    /// The "Settings" button.
    pub const WELCOME_SETTINGS: &str = "welcome-settings";
    /// The 1 dp divider above the welcome page's bottom bar.
    pub const WELCOME_DIVIDER: &str = "welcome-divider";
    /// The recent repositories list.
    pub const RECENT_REPOS: &str = "recent-repos";
    /// The recent repositories' filter field (inner ids `recent-filter-field`).
    pub const RECENT_FILTER: &str = "recent-filter";
    /// The filter field's clear button.
    pub const RECENT_FILTER_CLEAR: &str = "recent-filter-clear";
    /// The text shown when there are no recent repositories.
    pub const RECENT_EMPTY: &str = "recent-empty";
    /// The scrolling list of recent repositories.
    pub const RECENT_LIST: &str = "recent-list";
    /// A recent repository row (`(RECENT_ROW, index)`).
    pub const RECENT_ROW: &str = "recent-row";
    /// The remove button of a recent repository row (`(RECENT_REMOVE, index)`).
    pub const RECENT_REMOVE: &str = "recent-remove";
    /// The "Opening repository" screen.
    pub const LOADING: &str = "loading-repository";
    /// The "Opening repository" title.
    pub const LOADING_TITLE: &str = "loading-title";
    /// The path below the "Opening repository" title.
    pub const LOADING_PATH: &str = "loading-path";
    /// The processing overlay's content.
    pub const PROCESSING: &str = "processing";
    /// The processing overlay's title.
    pub const PROCESSING_TITLE: &str = "processing-title";
    /// The processing overlay's subtitle.
    pub const PROCESSING_SUBTITLE: &str = "processing-subtitle";
    /// The processing overlay's progress bar.
    pub const PROCESSING_BAR: &str = "processing-bar";
    /// The processing overlay's progress phase line.
    pub const PROCESSING_PHASE: &str = "processing-phase";
    /// The processing overlay's Cancel button.
    pub const PROCESSING_CANCEL: &str = "processing-cancel";
    /// A repository tab (`(TAB, index)`).
    pub const TAB: &str = "tab";
    /// The close button of a tab (`(TAB_CLOSE, index)`).
    pub const TAB_CLOSE: &str = "tab-close";
    /// The 3 dp underline of the selected tab.
    pub const TAB_UNDERLINE: &str = "tab-underline";
    /// The "+" button.
    pub const TAB_ADD: &str = "tab-add";
    /// The horizontally scrolling tab row.
    pub const TAB_SCROLL: &str = "tab-strip-scroll";
    /// The tab strip's horizontal scrollbar (only while the tabs overflow).
    pub const TAB_SCROLLBAR: &str = "tab-strip-scrollbar";
    /// The user identity at the left of the bottom bar.
    pub const BOTTOM_IDENTITY: &str = "bottom-identity";
    /// The "Version x.y" link at the right of the bottom bar.
    pub const BOTTOM_VERSION: &str = "bottom-version";
}

/// Sizes of the shell that no other region shares (dp), each with its spec section.
pub mod dims {
    /// Width of a welcome tile and an icon text button (07 §4.3).
    pub const WELCOME_BUTTON_W: f32 = 280.;
    /// Gap between the buttons and the recent list (07 §4.2: spacedBy 16).
    pub const WELCOME_COLUMN_GAP: f32 = 16.;
    /// Extra start padding of the recent list (07 §4.2).
    pub const WELCOME_RECENT_PAD_START: f32 = 32.;
    /// Width of the recent list (07 §4.2).
    pub const RECENT_W: f32 = 600.;
    /// Height of the recent list (07 §4.2).
    pub const RECENT_H: f32 = 400.;
    /// Width of the whole welcome group: 280 + 16 + 32 + 600 (07 §4.2).
    pub const WELCOME_GROUP_W: f32 =
        WELCOME_BUTTON_W + WELCOME_COLUMN_GAP + WELCOME_RECENT_PAD_START + RECENT_W;
    /// Vertical gap between the welcome title and the content row (07 §4.2).
    pub const WELCOME_ROW_GAP: f32 = 16.;
    /// Space below the welcome title (07 §4.2).
    pub const WELCOME_TITLE_PAD_BOTTOM: f32 = 8.;
    /// Space below each welcome tile (07 §4.3).
    pub const WELCOME_TILE_PAD_BOTTOM: f32 = 8.;
    /// Space above the "Additional information" heading (07 §4.3).
    pub const WELCOME_MORE_PAD_TOP: f32 = 16.;
    /// Space below the "Additional information" heading (07 §4.3).
    pub const WELCOME_MORE_PAD_BOTTOM: f32 = 8.;
    /// Share of the free vertical space above the welcome group: 25 % (07 §4.1: bias -0.5).
    pub const WELCOME_TOP_SHARE: f32 = 1.;
    /// Share of the free vertical space below the welcome group: 75 % (07 §4.1).
    pub const WELCOME_BOTTOM_SHARE: f32 = 3.;
    /// Height of a recent repository row: 8 + 24 + 8 (07 §4.4).
    pub const RECENT_ROW_H: f32 = 40.;
    /// Horizontal padding of a recent repository row (07 §4.4).
    pub const RECENT_ROW_PAD_H: f32 = 16.;
    /// Gap between the name and the parent path of a recent row (07 §4.4).
    pub const RECENT_ROW_GAP: f32 = 8.;
    /// Size of the remove button of a recent row (07 §4.4).
    pub const RECENT_REMOVE: f32 = 24.;
    /// Space between the filter field and the list (07 §4.4).
    pub const RECENT_LIST_PAD_TOP: f32 = 4.;
    /// Size of the filter field's clear button.
    pub const RECENT_CLEAR: f32 = 24.;
    /// Right inset of the clear button inside the filter field.
    pub const RECENT_CLEAR_INSET: f32 = 8.;
    /// Space above the "nothing to see here" text (07 §4.4).
    pub const RECENT_EMPTY_PAD_TOP: f32 = 16.;
    /// Width of a tab's close hit box: 8 + 14 + 8 (07 §2.2).
    pub const TAB_CLOSE_W: f32 = 30.;
    /// Maximum tab width: 16 + 720 + 30 (07 §2.2).
    pub const TAB_MAX_W: f32 = 766.;
    /// How close to the strip edge a dragged tab starts to scroll the strip (21 §4.5).
    pub const TAB_AUTOSCROLL_EDGE: f32 = 24.;
    /// Opacity of the tab drag ghost (21 §4.5).
    pub const TAB_GHOST_OPACITY: f32 = 0.9;
    /// Size of the "Opening repository" title (07 §3.1).
    pub const LOADING_TITLE: f32 = 36.;
    /// Size of the path below it (07 §3.1).
    pub const LOADING_PATH: f32 = 24.;
    /// Space below the processing title (07 §3.2).
    pub const PROCESSING_TITLE_PAD_BOTTOM: f32 = 8.;
    /// Space below the processing subtitle (07 §3.2).
    pub const PROCESSING_SUBTITLE_PAD_BOTTOM: f32 = 32.;
    /// Space below the processing progress bar (07 §3.2).
    pub const PROCESSING_BAR_PAD_BOTTOM: f32 = 32.;
    /// Space below the phase line, when a Cancel button follows.
    pub const PROCESSING_PHASE_PAD_BOTTOM: f32 = 16.;
    /// Space between the update button and the version link (07 §7).
    pub const BOTTOM_UPDATE_PAD_END: f32 = 16.;
}

/// The project page (the welcome page's "Source code" button); "Report a bug" opens its issues.
pub const REPOSITORY_URL: &str = "https://github.com/gitcthulhu/gitcthulhu";

gpui_kit::actions!(
    gitcthulhu_shell,
    [
        /// Moves the highlight of the recent repositories up.
        RecentUp,
        /// Moves the highlight of the recent repositories down.
        RecentDown,
    ]
);

/// Key context of the recent repositories list.
pub const RECENT_CONTEXT: &str = "RecentRepos";

/// Registers the shell's key bindings and actions (G1): Up/Down move through the recent repositories, also while
/// the (single-line) filter field has focus, whose own Up/Down would otherwise win.
pub fn register(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", RecentUp, Some(RECENT_CONTEXT)),
        KeyBinding::new("down", RecentDown, Some(RECENT_CONTEXT)),
        KeyBinding::new("up", RecentUp, Some("RecentRepos > Input")),
        KeyBinding::new("down", RecentDown, Some("RecentRepos > Input")),
    ]);
}
