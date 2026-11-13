//! Log paging (owned by G4 after G0b; spec 21 §4.2, 08 §2.11, 20 §3.5): when fewer than 200 loaded rows remain
//! below the viewport, the next 500 rows are read with the snapshot's `RepoModel::log_pager` in a `ReadSlot` and
//! appended with `ChunkedRows::appended`. A page is applied only while the snapshot generation it was requested for
//! is still current (a LOG reload replaces the pager and the generation).

use std::sync::Arc;

use gpui_kit::Context;

use super::LogView;
use crate::backend::{GitError, GitErrorKind};
use crate::notifications::ToastKind;
use crate::tab::ReadSlot;
use crate::tab::model::{LogRow, LogSnapshot};

/// Request more rows when fewer than this many remain below the viewport (08 §2.11).
pub const PAGE_THRESHOLD: usize = 200;
/// Rows per page request (08 §2.11).
pub const PAGE_ROWS: usize = 500;

/// Whether a page should be requested: `rows` layout rows are loaded and the last visible one is
/// `last_visible_layout` (exclusive end).
pub fn needs_page(rows: usize, last_visible_layout: usize, exhausted: bool) -> bool {
    !exhausted && rows.saturating_sub(last_visible_layout) < PAGE_THRESHOLD
}

/// The paging state of a log view.
#[derive(Default)]
pub struct Paging {
    slot: ReadSlot,
    /// The generation of the running request.
    requested: Option<u64>,
    /// A request of this generation failed: no further requests until the next reload.
    failed: Option<u64>,
}

impl Paging {
    /// A page request is running.
    pub fn is_running(&self) -> bool {
        self.requested.is_some() && self.slot.is_running()
    }

    /// Whether a request for `generation` may start now.
    pub fn can_request(&self, generation: u64) -> bool {
        !self.is_running() && self.failed != Some(generation)
    }

    /// A LOG reload: drops a running request of the old pager.
    pub fn reset(&mut self) {
        if self.requested.take().is_some() {
            self.slot.cancel();
        }
        self.failed = None;
    }
}

/// The snapshot with a page appended; `None` when `generation` is stale (the page is dropped).
pub fn append_page(
    old: &LogSnapshot,
    generation: u64,
    rows: Vec<LogRow>,
    max_lane: u16,
    exhausted: bool,
) -> Option<LogSnapshot> {
    (old.generation == generation).then(|| LogSnapshot {
        rows: old.rows.appended(rows),
        // The graph never narrows after paging (20 §3.4).
        max_lane: old.max_lane.max(max_lane),
        exhausted,
        generation,
    })
}

impl LogView {
    /// Starts the next page read if it is still needed (`last_visible_layout` = end of the visible layout rows).
    pub(super) fn request_page(&mut self, last_visible_layout: usize, cx: &mut Context<Self>) {
        let (snapshot, pager) = {
            let model = self.ctx.model.read(cx);
            (model.log.value.clone(), model.log_pager.clone())
        };
        let (Some(snapshot), Some(pager)) = (snapshot, pager) else {
            return;
        };
        let generation = snapshot.generation;
        if !needs_page(snapshot.rows.len(), last_visible_layout, snapshot.exhausted)
            || !self.paging.can_request(generation)
        {
            return;
        }
        self.paging.requested = Some(generation);
        self.paging.slot.start(
            cx,
            |v: &mut LogView| &mut v.paging.slot,
            move |cancel| pager.lock().next_rows(PAGE_ROWS, cancel),
            move |view, result, cx| view.apply_page(generation, result, cx),
        );
        cx.notify();
    }

    fn apply_page(
        &mut self,
        generation: u64,
        result: Result<(Vec<LogRow>, u16, bool), GitError>,
        cx: &mut Context<Self>,
    ) {
        self.paging.requested = None;
        match result {
            Ok((rows, max_lane, exhausted)) => {
                self.ctx.model.update(cx, |m, cx| {
                    let Some(old) = m.log.value.clone() else {
                        return;
                    };
                    if let Some(next) = append_page(&old, generation, rows, max_lane, exhausted) {
                        m.set_log(Arc::new(next), cx);
                    }
                });
            }
            Err(err) if err.kind() == GitErrorKind::Cancelled => {}
            Err(err) => {
                self.paging.failed = Some(generation);
                let text = format!("Loading more commits failed: {err}");
                tracing::warn!("{text}");
                crate::app_view::show_toast(cx, Some(self.ctx.id), ToastKind::Error, text, None);
            }
        }
        cx.notify();
    }
}
