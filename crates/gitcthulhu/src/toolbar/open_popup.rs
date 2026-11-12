//! The toolbar's Open popup (owned by G2 after G0b; 07 §5.2). Also opened by the `OpenRepository` action in a
//! repository tab.

use gpui_kit::{App, Window};

use crate::notifications::ToastKind;
use crate::tab::TabCtx;

/// Opens the Open popup of the tab of `ctx` (placeholder: an Info toast until G2 implements it).
pub fn open(ctx: &TabCtx, _window: &mut Window, cx: &mut App) {
    crate::app_view::show_toast(
        cx,
        Some(ctx.id),
        ToastKind::Positive,
        "Not implemented yet: Open popup",
        None,
    );
}
