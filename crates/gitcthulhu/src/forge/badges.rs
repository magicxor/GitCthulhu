//! PR / CI badges (D0.1; owned by X4 after G0b; spec 21 §5.5). Rows only read the controller's badge map; never a
//! forge call during render. Milestone 1: no badges.

use gpui_kit::{AnyElement, App};

use crate::backend::FullName;
use crate::tab::TabCtx;

/// The `[PR badge][CI dot]` trailing content of a local-branch row in the side panel, before "HEAD".
/// Milestone 1: `None`.
pub fn branch_row_badge(_ctx: &TabCtx, _branch: &FullName, _cx: &App) -> Option<AnyElement> {
    None
}

/// The PR segment at the end of a local-branch chip in the log. Milestone 1: `None`.
pub fn chip_segment(_ctx: &TabCtx, _branch: &FullName, _cx: &App) -> Option<AnyElement> {
    None
}
