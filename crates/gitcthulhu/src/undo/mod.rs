//! Undo / Redo UI (D0.3; owned by X2; spec 21 §5.1, 17 §4.4-§4.5): the toolbar buttons with tooltips that name the
//! operation, the history popover with restore-to, the "Undo" / "Redo" actions of success toasts and the recovery
//! prompt of an interrupted undo. The data comes from `model.undo` (the oplog's cached state, reloaded after every
//! recorded operation); every restore runs through `RepoTab::{undo, redo, restore_to}` (`run_restore`: the guard is
//! released before each confirmation).
//!
//! The per-tab [`UndoController`] is the view of the toolbar slot (the frame constructs it with the tab). It
//! tracks whether an operation runs (events of the tab), owns the popover and keeps the small bookkeeping that
//! makes the toast actions safe: a toast's "Undo" only acts while the operation it belongs to is still the newest
//! one (see [`toast_action`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, ParentElement as _, Render, Styled as _, Subscription,
    WeakEntity, Window, anchored, deferred, div, point, px,
};

use crate::app_view::show_toast;
use crate::backend::present::RecoveryFlow;
use crate::backend::{EntryId, RefreshSet};
use crate::icons::AppIcon;
use crate::metrics::{self, dpx};
use crate::notifications::{ToastAction, ToastKind};
use crate::tab::model::RepoModelEvent;
use crate::tab::ops::{OpKind, RestoreKind};
use crate::tab::{RepoTabEvent, TabCtx, TabId};
use crate::widgets::{BoundsCell, MenuButton, track_bounds};

pub mod history;
mod split;
pub mod texts;

#[cfg(feature = "qa")]
mod qa;
#[cfg(test)]
mod tests;

use history::{ClosePopover, HistoryPopover, POPOVER_CONTEXT, PopoverEvent};
use split::UndoSplit;
use texts::{ButtonTip, Which};

/// Element ids of the undo UI.
pub mod ids {
    /// The Undo split button.
    pub const TOOLBAR_UNDO: &str = "toolbar-undo";
    /// The Redo button.
    pub const TOOLBAR_REDO: &str = "toolbar-redo";
    /// The history popover.
    pub const UNDO_HISTORY: &str = "undo-history";
    /// The virtualised list of the popover.
    pub const UNDO_HISTORY_LIST: &str = "undo-history-list";
    /// A row of the list (`(id, index)`).
    pub const UNDO_HISTORY_ROW: &str = "undo-history-row";
    /// The text shown instead of the list (loading, empty, failed).
    pub const UNDO_HISTORY_EMPTY: &str = "undo-history-empty";
    /// "Clear undo history" in the footer.
    pub const UNDO_HISTORY_CLEAR: &str = "undo-history-clear";
    /// The footer note.
    pub const UNDO_HISTORY_NOTE: &str = "undo-history-note";
}

/// Sizes of the popover (21 §5.1) and its rows; the toolbar buttons keep the foundation's constants.
pub(crate) mod dims {
    /// Popover width.
    pub const POPOVER_W: f32 = 360.; // 21 §5.1
    /// Popover height.
    pub const POPOVER_H: f32 = 420.; // 21 §5.1
    /// Row height.
    pub const ROW_H: f32 = 36.; // 21 §5.1
    /// Kind icon size.
    pub const ROW_ICON: f32 = 16.; // 21 §5.1
    /// Space between the parts of a row.
    pub const ROW_GAP: f32 = 8.;
    /// Horizontal padding of rows, messages and the footer.
    pub const ROW_PAD_H: f32 = 12.;
    /// Horizontal padding of a status badge.
    pub const BADGE_PAD_H: f32 = 6.;
    /// Vertical padding of the footer.
    pub const FOOTER_PAD_V: f32 = 8.;
    /// Space between the footer's link and note.
    pub const FOOTER_GAP: f32 = 4.;
    /// Border above the footer.
    pub const FOOTER_BORDER: f32 = 1.;
    /// The chevron's left edge inside the 64 dp Undo button: the popover is anchored at its bottom-left.
    pub const CHEVRON_LEFT: f32 = 48.;
}

/// Tooltip of the buttons of a tab without a controller (never in the app; probes without a repository tab).
pub const FALLBACK_TOOLTIP: &str = texts::NOTHING_TO_UNDO;

/// The controllers of the open tabs, so that [`toolbar_slot`] (which only gets a `TabCtx`) finds the view.
#[derive(Default)]
struct Controllers(HashMap<TabId, WeakEntity<UndoController>>);

impl Global for Controllers {}

/// What the controller knows about the newest operation of a tab; read by the toast actions, which are built
/// without access to the app.
#[derive(Clone, Debug, Default)]
struct OpTrack {
    /// Number of operations started in the tab.
    epoch: u64,
    /// A recorded operation finished and the reload of the undo state has not arrived yet: `model.undo` is old.
    stale_model: bool,
    /// The two stack tops (`reverses` ids) when the newest operation started; `None` when `model.undo` was stale.
    before: Option<Tops>,
}

/// The entries that Undo and Redo would reverse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tops {
    undo: Option<EntryId>,
    redo: Option<EntryId>,
}

thread_local! {
    static TRACKS: RefCell<HashMap<TabId, OpTrack>> = RefCell::new(HashMap::new());
}

fn track<R>(id: TabId, f: impl FnOnce(&mut OpTrack) -> R) -> R {
    TRACKS.with(|tracks| f(tracks.borrow_mut().entry(id).or_default()))
}

fn tops_of(ctx: &TabCtx, cx: &App) -> Tops {
    let model = ctx.model.read(cx);
    let state = model.undo.as_ref();
    Tops {
        undo: state.and_then(|s| s.undo.as_ref()).map(|i| i.reverses),
        redo: state.and_then(|s| s.redo.as_ref()).map(|i| i.reverses),
    }
}

/// The per-tab undo controller: the view of the toolbar slot and the owner of the history popover.
pub struct UndoController {
    ctx: TabCtx,
    /// An operation of the tab runs (`OpStarted` .. `OpFinished`).
    busy: bool,
    popover: Option<Entity<HistoryPopover>>,
    popover_subscription: Option<Subscription>,
    bounds: BoundsCell,
    /// The recovery question of an interrupted undo was asked for the current interruption.
    asked_recovery: bool,
    page_size: usize,
    _subscriptions: Vec<Subscription>,
}

impl UndoController {
    /// The controller of the tab of `ctx`.
    pub fn new(ctx: TabCtx, cx: &mut Context<UndoController>) -> UndoController {
        let subscriptions = vec![
            cx.subscribe(&ctx.model, |this, _, ev: &RepoModelEvent, cx| {
                if ev.0.intersects(RefreshSet::UNDO) {
                    track(this.ctx.id, |t| t.stale_model = false);
                    cx.notify();
                }
            }),
            cx.subscribe(&ctx.events, |this, _, ev: &RepoTabEvent, cx| match ev {
                RepoTabEvent::OpStarted { .. } => {
                    let tops = tops_of(&this.ctx, cx);
                    track(this.ctx.id, |t| {
                        t.epoch += 1;
                        t.before = (!t.stale_model).then_some(tops);
                    });
                    this.busy = true;
                    cx.notify();
                }
                RepoTabEvent::OpFinished { .. } => {
                    this.busy = false;
                    cx.notify();
                }
                _ => {}
            }),
        ];
        let id = ctx.id;
        let me = cx.entity_id();
        let weak = cx.weak_entity();
        cx.default_global::<Controllers>().0.insert(id, weak);
        cx.on_release(move |_, cx| {
            let controllers = cx.default_global::<Controllers>();
            if controllers
                .0
                .get(&id)
                .is_some_and(|weak| weak.entity_id() == me)
            {
                controllers.0.remove(&id);
            }
            TRACKS.with(|tracks| tracks.borrow_mut().remove(&id));
        })
        .detach();
        UndoController {
            ctx,
            busy: false,
            popover: None,
            popover_subscription: None,
            bounds: Default::default(),
            asked_recovery: false,
            page_size: history::PAGE_SIZE,
            _subscriptions: subscriptions,
        }
    }

    /// The tab of this controller.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The history popover, while it is open.
    pub fn history(&self) -> Option<&Entity<HistoryPopover>> {
        self.popover.as_ref()
    }

    /// Whether the history popover is open.
    pub fn history_open(&self) -> bool {
        self.popover.is_some()
    }

    /// Overrides the page size of the popover (tests page with a handful of operations).
    #[cfg(test)]
    pub(crate) fn set_page_size(&mut self, page_size: usize) {
        self.page_size = page_size;
    }

    /// The enabled state and tooltip of the two buttons for the current model.
    pub fn tips(&self, cx: &App) -> (ButtonTip, ButtonTip) {
        let model = self.ctx.model.read(cx);
        let (state, unavailable) = (model.undo.as_ref(), model.undo_unavailable.as_ref());
        (
            texts::button_tip(Which::Undo, state, unavailable, self.busy),
            texts::button_tip(Which::Redo, state, unavailable, self.busy),
        )
    }

    /// Whether the oplog reports an interrupted undo or redo.
    fn interrupted(&self, cx: &App) -> bool {
        self.ctx
            .model
            .read(cx)
            .undo
            .as_ref()
            .is_some_and(|s| s.interrupted_restore)
    }

    /// Opens the history popover (a second call keeps the open one). While an undo is interrupted it asks the
    /// Finish / Roll back question instead: nothing else may run before that.
    pub fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.interrupted(cx) {
            self.ask_recovery(window, cx);
            return;
        }
        if let Some(popover) = &self.popover {
            popover.update(cx, |popover, cx| popover.focus(window, cx));
            return;
        }
        let ctx = self.ctx.clone();
        let page_size = self.page_size;
        let popover = cx.new(|cx| HistoryPopover::new(ctx, page_size, window, cx));
        self.popover_subscription =
            Some(cx.subscribe(&popover, |this, _, _: &PopoverEvent, cx| {
                this.close_history(cx);
            }));
        popover.update(cx, |popover, cx| popover.focus(window, cx));
        self.popover = Some(popover);
        cx.notify();
    }

    /// Closes the history popover.
    pub fn close_history(&mut self, cx: &mut Context<Self>) {
        if self.popover.take().is_some() {
            self.popover_subscription = None;
            cx.notify();
        }
    }

    fn ask_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = self.ctx.clone();
        window.defer(cx, move |window, cx| {
            // A restore that ends interrupted already asks through the runner's recovery flow; the reload that follows
            // must not stack the same question a second time (the chevron asks again later if needed).
            if window.has_active_dialog(cx) {
                return;
            }
            ctx.with_tab(window, cx, |tab, window, cx| {
                tab.run_recovery(RecoveryFlow::OplogInterrupted, window, cx);
            });
        });
    }

    fn popover_layer(&self, window: &Window, popover: Entity<HistoryPopover>) -> impl IntoElement {
        let origin = self.bounds.get().origin;
        // The popover's top-left corner is the chevron's bottom-left corner (21 §5.1).
        let at = point(
            origin.x + dpx(dims::CHEVRON_LEFT, window),
            origin.y + dpx(metrics::MENU_BUTTON, window),
        );
        let size = window.viewport_size();
        let close = |popover: Entity<HistoryPopover>| {
            move |_: &gpui_kit::MouseDownEvent, _: &mut Window, cx: &mut App| {
                popover.update(cx, |popover, cx| popover.dismiss(cx));
            }
        };
        let backdrop = div()
            .w(size.width)
            .h(size.height)
            .occlude()
            .on_mouse_down(MouseButton::Left, close(popover.clone()))
            .on_mouse_down(MouseButton::Right, close(popover.clone()));
        deferred(
            div()
                .absolute()
                .child(anchored().position(point(px(0.), px(0.))).child(backdrop))
                .child(anchored().position(at).snap_to_window().child(popover)),
        )
        .with_priority(1)
    }
}

impl Render for UndoController {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (undo, redo) = self.tips(cx);
        let interrupted = self.interrupted(cx);
        if interrupted && !self.asked_recovery {
            self.asked_recovery = true;
            self.ask_recovery(window, cx);
        } else if !interrupted {
            self.asked_recovery = false;
        }
        let history_available = self.ctx.model.read(cx).undo.is_some();
        let this = cx.weak_entity();
        let ctx = self.ctx.clone();
        let on_undo = {
            let ctx = ctx.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.undo(window, cx);
                })
            })
        };
        let on_redo = {
            move |window: &mut Window, cx: &mut App| {
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.redo(window, cx);
                })
            }
        };
        let popover = self
            .popover
            .clone()
            .map(|popover| self.popover_layer(window, popover));
        div()
            .relative()
            .flex()
            .flex_row()
            .flex_none()
            .child(track_bounds(&self.bounds))
            .child(UndoSplit {
                enabled: undo.enabled,
                chevron_enabled: history_available && !self.busy,
                tooltip: undo.text,
                hint: undo.hint,
                on_main: on_undo,
                on_chevron: Rc::new(move |window, cx| {
                    this.update(cx, |this, cx| {
                        if this.history_open() {
                            this.close_history(cx);
                        } else {
                            this.open_history(window, cx);
                        }
                    })
                    .ok();
                }),
            })
            .child({
                let mut redo_button = MenuButton::new(ids::TOOLBAR_REDO, AppIcon::RedoOp, "Redo")
                    .enabled(redo.enabled)
                    .tooltip(redo.text)
                    .on_click(on_redo);
                if let Some(hint) = redo.hint {
                    redo_button = redo_button.hint(hint);
                }
                redo_button
            })
            .when_some(popover, |root, layer| root.child(layer))
    }
}

/// The first child of the toolbar's centred group, before Pull: Undo (64 x 56 split button, margin end 4) and Redo
/// (56 x 56). They are enabled while the oplog can undo / redo and no operation runs; the tooltips name the
/// operation (17 §4.5) and the chevron opens the history popover.
pub fn toolbar_slot(ctx: &TabCtx, _window: &mut Window, cx: &mut App) -> AnyElement {
    match cx
        .try_global::<Controllers>()
        .and_then(|c| c.0.get(&ctx.id))
        .and_then(WeakEntity::upgrade)
    {
        Some(controller) => controller.into_any_element(),
        None => fallback_slot().into_any_element(),
    }
}

/// Both buttons disabled: the slot of a tab that has no controller.
fn fallback_slot() -> impl IntoElement {
    let nothing = |_: &mut Window, _: &mut App| {};
    div()
        .flex()
        .flex_row()
        .flex_none()
        .child(UndoSplit {
            enabled: false,
            chevron_enabled: false,
            tooltip: FALLBACK_TOOLTIP.into(),
            hint: None,
            on_main: Rc::new(nothing),
            on_chevron: Rc::new(nothing),
        })
        .child(
            MenuButton::new(ids::TOOLBAR_REDO, AppIcon::RedoOp, "Redo")
                .enabled(false)
                .tooltip(texts::NOTHING_TO_REDO),
        )
}

/// The `Undo` action of the repository frame (Ctrl+Z outside text inputs).
pub fn on_undo(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    shortcut(ctx, Which::Undo, window, cx);
}

/// The `Redo` action of the repository frame (Ctrl+Shift+Z, Ctrl+Y outside text inputs).
pub fn on_redo(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    shortcut(ctx, Which::Redo, window, cx);
}

fn shortcut(ctx: &TabCtx, which: Which, window: &mut Window, cx: &mut App) {
    let model = ctx.model.read(cx);
    if model.undo.is_none()
        && let Some(reason) = model.undo_unavailable.as_ref()
    {
        // The repository has no undo history at all: say why instead of opening an error dialog.
        let text = texts::unavailable_text(reason);
        show_toast(cx, Some(ctx.id), ToastKind::Warning, text, None);
        return;
    }
    ctx.with_tab(window, cx, |tab, window, cx| {
        match which {
            Which::Undo => tab.undo(window, cx),
            Which::Redo => tab.redo(window, cx),
        };
    });
}

/// What a success toast is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastContext {
    /// An operation that the oplog recorded.
    Recorded { kind: OpKind },
    /// A restore that completed.
    Restored { kind: RestoreKind },
}

/// Text of the warning shown when a toast's action no longer applies.
pub const STALE_TOAST: &str = "Something else happened since. Use the Undo and Redo buttons in the toolbar to step through the history.";

/// The action of a success toast: "Undo" after a recorded operation or a restore-to, "Redo" after an undo (17 §4.5).
///
/// A toast stays for a few seconds, and a successful operation that changed nothing is still reported as
/// recorded (the oplog drops its entry), so a plain `undo()` could reverse an older operation. The action
/// therefore acts only while no newer operation started and the relevant stack top differs from the one before the
/// toast's operation (the operation did push or pop an entry); otherwise it shows [`STALE_TOAST`].
pub fn toast_action(ctx: &TabCtx, what: ToastContext) -> Option<ToastAction> {
    let redo = matches!(
        what,
        ToastContext::Restored {
            kind: RestoreKind::Undo
        }
    );
    // The reload that follows this operation will tell whether it changed the stacks.
    let epoch = track(ctx.id, |t| {
        t.stale_model = true;
        t.epoch
    });
    let ctx = ctx.clone();
    Some(ToastAction {
        label: if redo { "Redo" } else { "Undo" }.into(),
        run: Rc::new(move |window, cx| {
            if !toast_applies(&ctx, epoch, redo, cx) {
                show_toast(cx, Some(ctx.id), ToastKind::Warning, STALE_TOAST, None);
                return;
            }
            ctx.with_tab(window, cx, |tab, window, cx| {
                if redo {
                    tab.redo(window, cx)
                } else {
                    tab.undo(window, cx)
                };
            });
        }),
    })
}

/// Whether the toast of the operation with `epoch` may still act (see [`toast_action`]).
fn toast_applies(ctx: &TabCtx, epoch: u64, redo: bool, cx: &App) -> bool {
    let now = tops_of(ctx, cx);
    track(ctx.id, |t| {
        if t.epoch != epoch || t.stale_model {
            return false;
        }
        let Some(before) = t.before else {
            return false;
        };
        let (before, now) = if redo {
            (before.redo, now.redo)
        } else {
            (before.undo, now.undo)
        };
        now.is_some() && now != before
    })
}

/// Registers the undo UI's bindings and hooks (X2).
pub fn register(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "escape",
        ClosePopover,
        Some(POPOVER_CONTEXT),
    )]);
    #[cfg(feature = "qa")]
    qa::register(cx);
}
