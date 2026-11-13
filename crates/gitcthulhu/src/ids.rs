//! Shared element-id constants for the application shell (spec 21 §1.1). Regions keep their own `ids` submodules.

/// Window content root.
pub const APP_ROOT: &str = "app-root";
/// Repository tab strip.
pub const TAB_STRIP: &str = "tab-strip";
/// Area below the tab strip that hosts the active tab's content.
pub const TAB_AREA: &str = "tab-area";
/// Repository frame (toolbar, split, bottom bar).
pub const REPO_FRAME: &str = "repo-frame";
/// Top toolbar.
pub const TOOLBAR: &str = "toolbar";
/// Three-pane split container.
pub const SPLIT: &str = "split";
/// Left pane of the split.
pub const PANE_FIRST: &str = "pane-first";
/// Centre pane of the split.
pub const PANE_CENTER: &str = "pane-center";
/// Right pane of the split.
pub const PANE_THIRD: &str = "pane-third";
/// Gutter between the first and the centre pane.
pub const GUTTER_FIRST: &str = "gutter-first";
/// Gutter between the centre and the third pane.
pub const GUTTER_THIRD: &str = "gutter-third";
/// Zero-height terminal panel slot (milestone 1) or the terminal panel itself.
pub const TERMINAL_PANEL: &str = "terminal-panel";
/// Divider between the split and the bottom bar.
pub const FRAME_DIVIDER: &str = "frame-divider";
/// Bottom info bar.
pub const BOTTOM_BAR: &str = "bottom-bar";
/// Toast layer.
pub const TOAST_LAYER: &str = "toast-layer";
/// Popup menu layer.
pub const MENU_LAYER: &str = "menu-layer";
/// Popup menu container.
pub const MENU: &str = "menu";
/// Dialog frame content.
pub const DIALOG_FRAME: &str = "dialog-frame";
