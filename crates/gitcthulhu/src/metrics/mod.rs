//! Shared layout metrics (spec 21 §3.5).
//!
//! All constants are in dp: with scale 1.0, `dp(38.) == px(38.)`. Element styles use [`dp`] (rems, so the Appearance
//! scale setting applies through `Theme::font_size`); painting and hit-testing code uses [`dpx`]. Region constants
//! live in the `metrics::<region>` submodules owned by the region packages.

use gpui_kit::{App, Pixels, Rems, Window, rems};

use crate::settings::LinesHeight;

pub mod blame;
pub mod dialogs;
pub mod diff;
pub mod history;
pub mod log;
pub mod rebase;
pub mod settings;
pub mod side_panel;
pub mod status;

/// Root rem size in dp: the kit `Theme::font_size` at scale 1.0.
pub const BASE_REM: f32 = 14.0;

/// A length in dp as `Rems`, for element styles. Scales with the rem size of the window.
pub fn dp(v: f32) -> Rems {
    rems(v / BASE_REM)
}

/// A length in dp as `Pixels` for painting and hit-math, resolved against the window's current rem size.
pub fn dpx(v: f32, window: &Window) -> Pixels {
    window.rem_size() * (v / BASE_REM)
}

/// Row heights that change with the "Lists spacing" setting (07 §13.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineHeights {
    /// File rows in status and commit-file lists.
    pub file_row: f32,
    /// Commit rows in the log.
    pub log_row: f32,
    /// Side-panel rows.
    pub side_item: f32,
}

/// Default "Spaced" row heights.
pub const SPACED: LineHeights = LineHeights {
    file_row: 38.,
    log_row: 38.,
    side_item: 36.,
};
/// "Compact" row heights.
pub const COMPACT: LineHeights = LineHeights {
    file_row: 34.,
    log_row: 34.,
    side_item: 34.,
};

impl LineHeights {
    /// The preset for a setting value.
    pub fn of(lines: LinesHeight) -> Self {
        match lines {
            LinesHeight::Spaced => SPACED,
            LinesHeight::Compact => COMPACT,
        }
    }
}

/// Row heights for the current settings.
pub fn line_heights(cx: &App) -> LineHeights {
    // Called per visible row per frame: read the setting in place instead of cloning `Settings`.
    let lines_height = cx
        .try_global::<crate::settings::SettingsGlobal>()
        .map(|g| g.0.lines_height)
        .unwrap_or_default();
    LineHeights::of(lines_height)
}

// window & shell - 07 §1, §2, §5, §7
pub const WINDOW_DEFAULT_W: f32 = 1280.; // 07 §1
pub const WINDOW_DEFAULT_H: f32 = 720.; // 07 §1
pub const TAB_STRIP_H: f32 = 36.; // 07 §2
pub const TAB_MIN_W: f32 = 200.; // 07 §2.2
pub const TAB_TITLE_MAX_W: f32 = 720.; // 07 §2.2
pub const TAB_TITLE_PAD_START: f32 = 16.; // 07 §2.2
pub const TAB_CLOSE: f32 = 14.; // 07 §2.2
pub const TAB_CLOSE_PAD_H: f32 = 8.; // 07 §2.2
pub const TAB_UNDERLINE_H: f32 = 3.; // 07 §2.2
pub const TAB_ADD: f32 = 36.; // 07 §2.3
pub const TAB_SCROLLBAR_H: f32 = 8.; // 07 §2.4
pub const TOOLBAR_H: f32 = 64.; // 07 §5.1.1
pub const TOOLBAR_PAD_V: f32 = 4.; // 07 §5.1.1
pub const TOOLBAR_EDGE: f32 = 16.; // 07 §5.1.1
pub const MENU_BUTTON: f32 = 56.; // 07 §5.1.2
pub const EXT_MENU_BUTTON_W: f32 = 64.; // 07 §5.1.3
pub const EXT_CHEVRON_W: f32 = 16.; // 07 §5.1.3
pub const TOOLBAR_ITEM_GAP: f32 = 4.; // 07 §5.1.1
pub const TOOLBAR_GROUP_GAP: f32 = 32.; // 07 §5.1.1
pub const TOOLBAR_ICON: f32 = 24.; // 07 §5.1.2
pub const SPLIT_GUTTER: f32 = 8.; // 07 §5.3
pub const SIDE_PANE_W: f32 = 220.; // 07 §5.3
pub const RIGHT_PANE_W: f32 = 330.; // 07 §5.3
pub const PANE_MIN_W: f32 = 150.; // 07 §5.3
pub const PANE_MAX_FRACTION: f32 = 0.4; // 07 §5.3
/// Right pane padding end/bottom (08 §1).
pub const PANE_EDGE_PAD: f32 = 8.;
pub const FRAME_DIVIDER_H: f32 = 1.; // 07 §7
pub const BOTTOM_BAR_H: f32 = 32.; // 07 §7
pub const BOTTOM_BAR_PAD_H: f32 = 16.; // 07 §7

// shared primitives - 07 §9, §10; 08 §0.5; 09 §0.3
pub const HEADER_BAR_H: f32 = 34.; // 08 §0.5
pub const FIELD_H: f32 = 38.; // 07 §10.1
pub const FIELD_BORDER: f32 = 2.; // 07 §10.1
pub const RADIUS: f32 = 4.; // 07 §13.5
pub const RADIUS_LG: f32 = 8.; // 07 §13.5
pub const ICON_SM: f32 = 16.; // 07 §9.3
pub const ICON_MD: f32 = 18.; // 08 §0.5
pub const MENU_MIN_W: f32 = 180.; // 07 §9.3
pub const MENU_ITEM_PAD: f32 = 8.; // 07 §9.3
pub const MENU_ICON_SLOT: f32 = 16.; // 07 §9.3
pub const MENU_ICON_GAP: f32 = 8.; // 07 §9.3
pub const MENU_SEPARATOR_H: f32 = 2.; // 07 §9.3
pub const MENU_SEPARATOR_PAD_H: f32 = 16.; // 07 §9.3
pub const MENU_SEPARATOR_PAD_V: f32 = 4.; // 07 §9.3
pub const TOOLTIP_GAP: f32 = 4.; // 07 §9.1
pub const TOOLTIP_PAD: f32 = 8.; // 07 §9.1
pub const TOOLTIP_DELAY_MS: u64 = 500; // 07 §9.2
pub const TOAST_BOTTOM: f32 = 48.; // 07 §8
pub const TOAST_MARGIN: f32 = 8.; // 07 §8
pub const TOAST_H: f32 = 36.; // 07 §8
pub const TOAST_BLOCK: f32 = 32.; // 07 §8
pub const TOAST_MS: u64 = 2000; // 07 §8
pub const DIALOG_PAD: f32 = 16.; // 09 §3.0
pub const DIALOG_ICON: f32 = 64.; // 09 §3.0
pub const SINGLE_FIELD_W: f32 = 300.; // 09 §3.0
pub const PROCESSING_DELAY_MS: u64 = 300; // 07 §3.2
pub const PROCESSING_BAR_W: f32 = 280.; // 07 §3.2
pub const PRIMARY_BUTTON_PAD_H: f32 = 12.; // 07 §10.7
pub const PRIMARY_BUTTON_PAD_V: f32 = 6.; // 07 §10.7
pub const SECONDARY_BUTTON_PAD_H: f32 = 16.; // 07 §7
pub const SECONDARY_BUTTON_PAD_V: f32 = 4.; // 07 §7
pub const SWITCH_W: f32 = 48.; // 07 §10.4
pub const SWITCH_H: f32 = 28.; // 07 §10.4
pub const CHECKBOX_SLOT: f32 = 28.; // 09 §0.3

// D0 additions - 21 §5
/// ExtendedMenuButton width of the Undo button.
pub const UNDO_BUTTON_W: f32 = 64.;
pub const TERMINAL_HEADER_H: f32 = 34.;
pub const TERMINAL_DEFAULT_H: f32 = 240.;
pub const TERMINAL_MIN_H: f32 = 120.;
/// The least height the split keeps above the terminal panel: the right pane's fixed content, so the Staged and
/// Unstaged headers with Stage all / Unstage all and the commit box never collapse (2 x (34 header + 4 list gap) +
/// 192 commit box + 8 bottom padding = 276; 21 §5.2, deviation in 21 §6).
pub const TERMINAL_MIN_SPLIT_H: f32 =
    2. * (HEADER_BAR_H + status::LIST_GAP) + status::COMMIT_BOX_H + PANE_EDGE_PAD;
pub const TERMINAL_TAB_MIN_W: f32 = 120.;
pub const TERMINAL_TAB_MAX_W: f32 = 240.;
pub const AI_BUTTON_H: f32 = 24.;
pub const PR_BADGE_H: f32 = 18.;
pub const CI_DOT: f32 = 8.;
pub const STATUS_DOT: f32 = 8.;
pub const UNDO_HISTORY_W: f32 = 360.;
pub const UNDO_HISTORY_H: f32 = 420.;

#[cfg(test)]
mod tests;
