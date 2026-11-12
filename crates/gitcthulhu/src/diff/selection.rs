//! Text selection in the diff (G7b; spec 21 §4.1, 09 §1.3 "Selection and copy", §1.4, §4.7).
//!
//! A bridge over gpui-base's window text selection: the whole diff list is ONE participant
//! (`TextSelectionHandle`), whose text cells report their runs while they are painted. Endpoints are content keys
//! (`SelKey` + offset in the line's display text), so a selection survives rows being scrolled out of the virtualised
//! list, and copying reads the source lines of the `TextDiff` (full lines, not the 2,000-byte display cut).
//!
//! Behaviour:
//! - Drag selects (word on double-click, line on triple-click: gpui-base); Shift-click extends.
//! - Split mode (09 §1.4): a press makes both sides selectable; after more than [`SIDE_LOCK_MOVES`] drag moves the
//!   side opposite to the press is excluded; a release after fewer moves makes both sides selectable again.
//! - `secondary-c` copies and `secondary-a` selects the whole document in the [`DIFF_CONTEXT`] key context
//!   ([`DiffSelection::key_actions`] on the focused diff root; [`register`]'s global handlers act on the diff painted
//!   last otherwise); without a diff selection the keystroke goes on to the next binding. The window's own copy
//!   binding (gpui-base `Root`) copies the same text when the diff is not focused.
//! - Right-click while text is selected opens the selection menu (09 §4.7: Copy, Select All); without a selection
//!   the press goes on to the diff-line menu.
//! - The selection is painted between the runs' backgrounds and their glyphs in `primary` at 40 % (09 §0.3).

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpui_kit::base::{
    TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::{
    AnyElement, App, FocusHandle, HitboxBehavior, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, StyledText, Subscription, Window,
    WindowId,
};

use self::geometry::{CellRun, Frame};
use self::range::{Selection, decode, encode};
use super::model::{DiffDoc, DiffMode};
use crate::theme::ActiveTokens as _;

mod cell;
mod geometry;
mod range;
#[cfg(test)]
mod tests;

/// The key context of the diff view; G7b binds copy and select-all in it.
pub const DIFF_CONTEXT: &str = "Diff";

/// In split mode, a drag of more than this many move events excludes the side opposite to the press (09 §1.4).
pub const SIDE_LOCK_MOVES: u32 = 5;

/// Opacity of the selection background over `primary` (Material's selection colour, 09 §0.3).
pub const SELECTION_ALPHA: f32 = 0.4;

gpui_kit::actions!(
    gitcthulhu_diff,
    [
        /// Copy the diff's selected text (full source lines).
        CopySelection,
        /// Select the whole diff document.
        SelectAll,
    ]
);

/// Which column a selected text run belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelSide {
    /// Split mode, the left (old) column.
    Old,
    /// Split mode, the right (new) column.
    New,
    /// Unified mode.
    Unified,
}

/// Stable key of a selectable text run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SelKey {
    /// Index into `TextDiff::lines`.
    pub line: u32,
    /// The column of the run.
    pub side: SelSide,
}

/// The press-drag-release state of the current gesture (09 §1.4 one-side rule).
#[derive(Clone, Copy, Debug, Default)]
struct Gesture {
    pressed: bool,
    moves: u32,
    press_side: Option<SelSide>,
    locked: Option<SelSide>,
}

/// The state shared by a [`DiffSelection`], its painted cells and the participant's callbacks.
pub(crate) struct Inner {
    handle: TextSelectionHandle,
    doc: Option<Arc<DiffDoc>>,
    select_all: bool,
    gesture: Gesture,
    frame: Frame,
    /// The selection as computed for the frame being painted.
    painted_selection: Selection,
    window: Option<WindowId>,
    refresh: Option<Subscription>,
}

type Shared = Rc<RefCell<Inner>>;

impl Inner {
    /// The current selection of the document, from the participant's endpoints (or select-all).
    fn selection(&self, cx: &App) -> Selection {
        let Some(doc) = self.doc.as_ref() else {
            return Selection::None;
        };
        if self.select_all {
            return if doc.texts.is_empty() {
                Selection::None
            } else {
                Selection::All
            };
        }
        let Some(snapshot) = self.handle.snapshot(cx) else {
            return Selection::None;
        };
        let own = Some(self.handle.entity_id());
        if snapshot.anchor().entity_id() != own || snapshot.cursor().entity_id() != own {
            return Selection::None;
        }
        let (Some(anchor), Some(cursor)) = (
            snapshot.anchor().content_key().and_then(decode),
            snapshot.cursor().content_key().and_then(decode),
        ) else {
            return Selection::None;
        };
        Selection::between(doc, anchor, cursor, self.gesture.locked)
    }

    fn selected_text(&self, cx: &App) -> Option<String> {
        let doc = self.doc.as_ref()?;
        self.selection(cx).text(doc)
    }

    /// The side a new endpoint is restricted to: the press side once the split-mode lock engaged.
    fn endpoint_side(&self) -> Option<SelSide> {
        let split = self.doc.as_ref().is_some_and(|d| d.mode == DiffMode::Split);
        if split { self.gesture.locked } else { None }
    }
}

thread_local! {
    /// The diff selection painted last: the target of the `Diff`-context actions (one main window, T20).
    static ACTIVE: RefCell<Weak<RefCell<Inner>>> = const { RefCell::new(Weak::new()) };
    /// Test clipboard: platform side effects panic on the test platform (T8).
    #[cfg(test)]
    static CLIPBOARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn set_active(shared: &Shared) {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if !std::ptr::eq(active.as_ptr(), Rc::as_ptr(shared)) {
            *active = Rc::downgrade(shared);
        }
    });
}

fn active() -> Option<Shared> {
    ACTIVE.with(|active| active.borrow().upgrade())
}

/// Writes `text` to the clipboard (recorded instead under `cfg(test)`).
fn write_clipboard(text: String, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
        CLIPBOARD.with(|clipboard| clipboard.borrow_mut().push(text));
    }
    #[cfg(not(test))]
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
}

/// The texts copied so far in this test thread.
#[cfg(test)]
pub(crate) fn take_clipboard() -> Vec<String> {
    CLIPBOARD.with(|clipboard| std::mem::take(&mut *clipboard.borrow_mut()))
}

/// Copies the selection of `shared`, if any.
fn copy(shared: &Shared, cx: &mut App) {
    let text = shared.borrow().selected_text(cx);
    if let Some(text) = text {
        write_clipboard(text, cx);
    }
}

/// The `CopySelection` action: copies the diff's selection, or lets the next binding of the keystroke (gpui-base
/// `Root`'s copy of the window selection) handle it when the diff has none.
fn copy_or_propagate(shared: &Shared, cx: &mut App) {
    let text = shared.borrow().selected_text(cx);
    match text {
        Some(text) => write_clipboard(text, cx),
        None => cx.propagate(),
    }
}

/// The `SelectAll` action: selects the whole document, or propagates without one.
fn select_all_or_propagate(shared: &Shared, cx: &mut App) {
    if !select_all(shared, cx) {
        cx.propagate();
    }
}

/// Selects the whole document of `shared`; `false` when there is no text to select.
fn select_all(shared: &Shared, cx: &mut App) -> bool {
    let handle = {
        let mut inner = shared.borrow_mut();
        if inner.doc.as_ref().is_none_or(|doc| doc.texts.is_empty()) {
            return false;
        }
        inner.select_all = true;
        inner.handle.clone()
    };
    handle.set_local_selection(true, cx);
    cx.refresh_windows();
    true
}

/// Clears the selection of `shared`, including the window selection when its endpoints are in this diff.
fn clear(shared: &Shared, cx: &mut App) {
    let (handle, window) = {
        let mut inner = shared.borrow_mut();
        inner.select_all = false;
        inner.gesture = Gesture::default();
        inner.painted_selection = Selection::None;
        (inner.handle.clone(), inner.window)
    };
    handle.set_local_selection(false, cx);
    let own = Some(handle.entity_id());
    let anchored_here = handle
        .snapshot(cx)
        .is_some_and(|snapshot| snapshot.anchor().entity_id() == own);
    if anchored_here && let Some(window) = window {
        TextSelection::clear_for_window(window, cx);
    }
}

/// The selection state of one diff view.
pub struct DiffSelection {
    shared: Shared,
    /// The document of the last [`DiffSelection::reset`] (mirrors `Inner::doc`, so [`DiffSelection::doc`] can lend
    /// it as the fixed interface does).
    doc: Option<Arc<DiffDoc>>,
}

impl DiffSelection {
    /// An empty selection.
    pub fn new(cx: &mut App) -> Self {
        let handle = TextSelectionHandle::new("", cx);
        let shared: Shared = Rc::new(RefCell::new(Inner {
            handle: handle.clone(),
            doc: None,
            select_all: false,
            gesture: Gesture::default(),
            frame: Frame::default(),
            painted_selection: Selection::None,
            window: None,
            refresh: None,
        }));
        let weak = Rc::downgrade(&shared);
        handle.resolve_content_key_with(
            {
                let weak = weak.clone();
                move |point, _cx| {
                    let shared = weak.upgrade()?;
                    let inner = shared.try_borrow().ok()?;
                    let window_point = point + inner.frame.origin;
                    let (key, offset) = inner.frame.key_at(window_point, inner.endpoint_side())?;
                    Some(encode(key, offset))
                }
            },
            cx,
        );
        handle.copy_with(
            {
                let weak = weak.clone();
                move |cx| {
                    weak.upgrade()
                        .and_then(|shared| {
                            let inner = shared.try_borrow().ok()?;
                            inner.selected_text(cx)
                        })
                        .unwrap_or_default()
                }
            },
            cx,
        );
        handle.clear_with(
            move |_cx| {
                if let Some(shared) = weak.upgrade()
                    && let Ok(mut inner) = shared.try_borrow_mut()
                {
                    inner.select_all = false;
                }
            },
            cx,
        );
        Self { shared, doc: None }
    }

    /// A new document (or a mode switch): clears the selection and remembers the document for copying. The same
    /// document again (a reload with unchanged blobs) keeps the selection.
    pub fn reset(&mut self, doc: &Arc<DiffDoc>, cx: &mut App) {
        if self
            .doc
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, doc))
        {
            return;
        }
        clear(&self.shared, cx);
        self.shared.borrow_mut().doc = Some(doc.clone());
        self.doc = Some(doc.clone());
    }

    /// Wraps a row's text cell so it takes part in selection: `text` must lay out the display text of line
    /// `key.line` (`DiffDoc::texts`). The cell reports its run while it paints and paints the selection behind its
    /// glyphs.
    pub fn text_cell(
        &self,
        key: SelKey,
        text: StyledText,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let window_id = window.window_handle().window_id();
        let display = {
            let mut inner = self.shared.borrow_mut();
            if inner.window != Some(window_id) {
                inner.window = Some(window_id);
                inner.refresh = None;
            }
            inner
                .doc
                .as_ref()
                .and_then(|doc| doc.texts.get(key.line as usize).cloned())
        };
        let color = cx.tokens().primary.opacity(SELECTION_ALPHA);
        cell::SelectableCell::new(key, text, display, self.shared.clone(), color).into_any_element()
    }

    /// Whether any text is selected.
    pub fn has_selection(&self, cx: &App) -> bool {
        self.shared.borrow().selection(cx).is_some()
    }

    /// The source text of the selection: full lines (not the 2,000-byte display cut), joined with '\n'.
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        self.shared.borrow().selected_text(cx)
    }

    /// Copies the selection to the clipboard (no-op without one).
    pub fn copy(&self, cx: &mut App) {
        copy(&self.shared, cx);
    }

    /// Selects the whole document.
    pub fn select_all(&self, cx: &mut App) {
        select_all(&self.shared, cx);
    }

    /// Clears the selection.
    pub fn clear(&mut self, cx: &mut App) {
        clear(&self.shared, cx);
    }

    /// The document the selection belongs to.
    pub fn doc(&self) -> Option<&Arc<DiffDoc>> {
        self.doc.as_ref()
    }

    /// Binds copy and select-all (`secondary-c` / `secondary-a` in [`DIFF_CONTEXT`]) on `element` to THIS selection.
    /// Put it on the focusable diff root that carries the `Diff` key context: the handlers then act on the view that
    /// has focus, not on the diff painted last (the global fallback of [`register`]). Without a selection, copy
    /// propagates to the window's own copy binding.
    pub fn key_actions<E: InteractiveElement>(&self, element: E) -> E {
        let (copy_target, all_target) = (Rc::downgrade(&self.shared), Rc::downgrade(&self.shared));
        element
            .on_action(
                move |_: &CopySelection, _, cx| match copy_target.upgrade() {
                    Some(shared) => copy_or_propagate(&shared, cx),
                    None => cx.propagate(),
                },
            )
            .on_action(move |_: &SelectAll, _, cx| match all_target.upgrade() {
                Some(shared) => select_all_or_propagate(&shared, cx),
                None => cx.propagate(),
            })
    }

    /// Focuses `focus` when a selection drag begins in the diff (gpui-base `focus_with`), so that `secondary-c` /
    /// `secondary-a` reach the `Diff` key context after the user selected with the mouse.
    pub fn focus_on_drag(&self, focus: &FocusHandle, cx: &mut App) {
        let focus = focus.clone();
        let handle = self.shared.borrow().handle.clone();
        handle.focus_with(move |window, cx| window.focus(&focus, cx), cx);
    }

    /// The selection menu (09 §4.7) of this selection, for a right-click handler.
    pub fn menu(&self, cx: &App) -> Vec<crate::menus::MenuItem> {
        menu_items(&self.shared, cx)
    }
}

/// The 09 §4.7 menu bound to `shared`. Copy takes the text selected when the menu opens: the press on the menu item
/// ends the window selection before the item runs.
fn menu_items(shared: &Shared, cx: &App) -> Vec<crate::menus::MenuItem> {
    let text = shared.borrow().selected_text(cx);
    let has_selection = text.is_some();
    let select_target = Rc::downgrade(shared);
    crate::menus::diff_selection::selection_menu(
        has_selection,
        Rc::new(move |_, cx| {
            if let Some(text) = text.clone() {
                write_clipboard(text, cx);
            }
        }),
        Rc::new(move |_, cx| {
            if let Some(shared) = select_target.upgrade() {
                select_all(&shared, cx);
            }
        }),
    )
}

/// Ends the collection of a frame's runs (first paint after the prepaints): registers the participant with this
/// frame's geometry, reports the runs, installs the gesture and menu listeners, and computes the selection to paint.
fn finish_frame(shared: &Shared, window: &mut Window, cx: &mut App) {
    set_active(shared);
    let (handle, registration, runs) = {
        let mut inner = shared.borrow_mut();
        inner.frame.painted = true;
        let Some(hitbox) = inner.frame.hitbox.clone() else {
            return;
        };
        let area = inner.frame.area.unwrap_or(hitbox.bounds);
        inner.frame.origin = area.origin;
        let text_bounds = inner.frame.runs.iter().map(|run| run.bounds).collect();
        let runs: Vec<TextSelectionRun> = inner
            .frame
            .runs
            .iter()
            .map(|run: &CellRun| {
                TextSelectionRun::new(run.text.clone(), run.layout.clone(), run.bounds)
                    .with_document_order(encode(run.key, 0).value())
            })
            .collect();
        (
            inner.handle.clone(),
            TextSelectionRegistration::new(hitbox, area).with_text_bounds(text_bounds),
            runs,
        )
    };
    let registration = registration.with_rendered_element(&handle, window, cx);
    handle.register(registration, window, cx);
    handle.update_runs(&runs, cx);
    if shared.borrow().refresh.is_none() {
        let subscription = handle.refresh_window_on_change(window, cx);
        shared.borrow_mut().refresh = Some(subscription);
    }
    let selection = shared.borrow().selection(cx);
    shared.borrow_mut().painted_selection = selection;
    install_listeners(shared, window);
}

/// The gesture listeners of the 09 §1.4 one-side rule and the 09 §4.7 menu, for this frame.
fn install_listeners(shared: &Shared, window: &mut Window) {
    let Some(hitbox) = shared.borrow().frame.hitbox.clone() else {
        return;
    };
    let weak = Rc::downgrade(shared);
    window.on_mouse_event({
        let weak = weak.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.capture() || !hitbox.is_hovered(window) {
                return;
            }
            let Some(shared) = weak.upgrade() else {
                return;
            };
            match event.button {
                MouseButton::Left => {
                    let Ok(mut inner) = shared.try_borrow_mut() else {
                        return;
                    };
                    let side = inner.frame.side_at(event.position);
                    inner.gesture = Gesture {
                        pressed: event.click_count == 1,
                        moves: 0,
                        press_side: side,
                        locked: None,
                    };
                }
                MouseButton::Right => {
                    let has_selection = shared
                        .try_borrow()
                        .is_ok_and(|inner| inner.selection(cx).is_some());
                    if has_selection {
                        let items = menu_items(&shared, cx);
                        crate::menus::open_context_menu(window, cx, event.position, items);
                        cx.stop_propagation();
                    }
                }
                _ => {}
            }
        }
    });
    window.on_mouse_event({
        let weak = weak.clone();
        move |event: &MouseMoveEvent, phase, _, _| {
            if !phase.capture() || event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let Ok(mut inner) = shared.try_borrow_mut() else {
                return;
            };
            if !inner.gesture.pressed {
                return;
            }
            inner.gesture.moves = inner.gesture.moves.saturating_add(1);
            let split = inner
                .doc
                .as_ref()
                .is_some_and(|d| d.mode == DiffMode::Split);
            if split && inner.gesture.moves > SIDE_LOCK_MOVES && inner.gesture.locked.is_none() {
                inner.gesture.locked = inner.gesture.press_side;
            }
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, _| {
        if !phase.capture() || event.button != MouseButton::Left {
            return;
        }
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let Ok(mut inner) = shared.try_borrow_mut() else {
            return;
        };
        if inner.gesture.pressed {
            inner.gesture.pressed = false;
            if inner.gesture.moves <= SIDE_LOCK_MOVES {
                inner.gesture.locked = None;
            }
        }
    });
}

/// Registers the selection's key bindings (`secondary-c` / `secondary-a` in the `Diff` context) and their handlers.
pub fn register(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-c", CopySelection, Some(DIFF_CONTEXT)),
        KeyBinding::new("secondary-a", SelectAll, Some(DIFF_CONTEXT)),
    ]);
    // The fallback when the focused diff root has no `DiffSelection::key_actions`: the diff painted last.
    cx.on_action(|_: &CopySelection, cx: &mut App| match active() {
        Some(shared) => copy_or_propagate(&shared, cx),
        None => cx.propagate(),
    });
    cx.on_action(|_: &SelectAll, cx: &mut App| match active() {
        Some(shared) => select_all_or_propagate(&shared, cx),
        None => cx.propagate(),
    });
}

/// Inserts this frame's participant hitbox; kept here so that `cell` only reports geometry.
fn insert_hitbox(frame: &mut Frame, window: &mut Window) {
    let mask = window.content_mask().bounds;
    let area = frame.area.map_or(mask, |area| area.union(&mask));
    frame.area = Some(area);
    frame.hitbox = Some(window.insert_hitbox(area, HitboxBehavior::Normal));
}
