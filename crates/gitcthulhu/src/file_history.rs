//! The file history view (owned by G8 after G0b; 09 §2.2). The tab creates it while `RepoUi::history` is set; it
//! replaces the split area under the toolbar. The placeholder fills that area.

use gpui_kit::{App, Context, IntoElement, Render, Styled as _, Window};

use crate::backend::RelaPath;
use crate::tab::TabCtx;
use crate::widgets::observed;

/// Element ids of the file history view.
pub mod ids {
    /// The file history view.
    pub const FILE_HISTORY: &str = "file-history";
}

/// The history of one file.
pub struct FileHistoryView {
    ctx: TabCtx,
    path: RelaPath,
}

impl FileHistoryView {
    /// The history of `path` in the tab of `ctx`.
    pub fn new(ctx: TabCtx, path: RelaPath, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { ctx, path }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The file whose history is shown.
    pub fn path(&self) -> &RelaPath {
        &self.path
    }
}

impl Render for FileHistoryView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        observed(ids::FILE_HISTORY).size_full()
    }
}

/// Registers the file history view's key bindings (G8).
pub fn register(_cx: &mut App) {}
