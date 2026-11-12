//! Forge integration (D0.1; owned by X4 after G0b; spec 21 §5.5, §2.10, §2.13.3): per-tab controller (detection,
//! polling, badge map), badges, accounts. Milestone 1: no controller, no badges, no menu items.

use gpui_kit::{App, Entity};

use crate::tab::TabCtx;

pub mod accounts;
pub mod badges;

/// The per-tab forge controller (X4).
pub struct ForgeController {
    ctx: TabCtx,
}

impl ForgeController {
    /// The controller of the tab of `ctx`.
    pub fn new(ctx: TabCtx) -> Self {
        Self { ctx }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }
}

/// Creates the tab's forge controller when the repository has a forge. Milestone 1: `None`.
pub fn create_controller(_ctx: &TabCtx, _cx: &mut App) -> Option<Entity<ForgeController>> {
    None
}

/// Registers the forge menu extenders and actions (X4).
pub fn register(_cx: &mut App) {}
