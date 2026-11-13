//! `ReadSlot`: cancellable, stale-rejecting background reads (spec 21 §2.7, probe `read_slot_rejects_stale_results`).

use gpui_kit::{AppContext as _, AsyncApp, Context, Task, WeakEntity};

use crate::backend::CancelToken;

/// One kind of read of a view (a category loader, a diff load, a page request). A new start cancels the previous
/// read; only the newest result is applied.
#[derive(Default)]
pub struct ReadSlot {
    rev: u64,
    cancel: Option<CancelToken>,
    task: Option<Task<()>>,
}

impl ReadSlot {
    /// Cancels the previous read, then runs `work` in the background; `apply` runs on the UI thread only if no newer
    /// start (and no `cancel`) happened meanwhile. `slot` re-borrows this slot from the entity on completion.
    pub fn start<V: 'static, T: Send + 'static>(
        &mut self,
        cx: &mut Context<V>,
        slot: fn(&mut V) -> &mut ReadSlot,
        work: impl FnOnce(&CancelToken) -> T + Send + 'static,
        apply: impl FnOnce(&mut V, T, &mut Context<V>) + 'static,
    ) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.rev += 1;
        let rev = self.rev;
        let cancel = CancelToken::new();
        self.cancel = Some(cancel.clone());
        let bg = cx.background_spawn(async move { work(&cancel) });
        self.task = Some(
            cx.spawn(async move |this: WeakEntity<V>, cx: &mut AsyncApp| {
                let value = bg.await;
                let _ = this.update(cx, |v, cx| {
                    let slot = slot(v);
                    if slot.rev != rev {
                        return;
                    }
                    slot.cancel = None;
                    apply(v, value, cx)
                });
            }),
        );
    }

    /// Cancels the running read and bumps the revision so a late result is dropped.
    pub fn cancel(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();
        }
        self.rev += 1;
    }

    /// A read is running.
    pub fn is_running(&self) -> bool {
        self.cancel.is_some()
    }

    /// The revision of the newest start (tests, diagnostics).
    pub fn revision(&self) -> u64 {
        self.rev
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::{Context, IntoElement, Render, TestAppContext, Window, div};

    use super::*;

    struct Reader {
        slot: ReadSlot,
        applied: Vec<u32>,
    }

    impl Render for Reader {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn read_slot_drops_stale_result(cx: &mut TestAppContext) {
        let view = cx.new(|_| Reader {
            slot: ReadSlot::default(),
            applied: vec![],
        });
        let first_cancel = view.update(cx, |v, cx| {
            let mut seen = None;
            v.slot
                .start(cx, |v| &mut v.slot, |_| 1u32, |v, t, _| v.applied.push(t));
            seen.replace(v.slot.cancel.clone().expect("running"));
            v.slot
                .start(cx, |v| &mut v.slot, |_| 2u32, |v, t, _| v.applied.push(t));
            seen.expect("first token")
        });
        assert!(
            first_cancel.is_cancelled(),
            "a new start cancels the previous read"
        );
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.applied.clone()),
            vec![2],
            "only the newest result is applied"
        );
        assert!(
            !view.read_with(cx, |v, _| v.slot.is_running()),
            "the slot is idle after applying"
        );

        view.update(cx, |v, cx| {
            v.slot
                .start(cx, |v| &mut v.slot, |_| 3u32, |v, t, _| v.applied.push(t));
            v.slot.cancel();
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.applied.clone()),
            vec![2],
            "a cancelled read is never applied"
        );
    }
}
