//! Text selection in the diff (owned by G7b after G0b; spec 21 §4.1, 09 §1.4): a bridge over gpui-base's
//! `TextSelectionHandle` with stable keys per line and side, copy of full source lines, and the split-mode one-side
//! rule. The milestone-1 stub selects nothing; the interface is final.

use std::sync::Arc;

use gpui_kit::{AnyElement, App, IntoElement, StyledText, Window};

use super::model::DiffDoc;

/// Which column a selected text run belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelSide {
    Old,
    New,
    Unified,
}

/// Stable key of a selectable text run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SelKey {
    /// Index into `TextDiff::lines`.
    pub line: u32,
    pub side: SelSide,
}

/// The selection state of one diff view.
#[derive(Default)]
pub struct DiffSelection {
    doc: Option<Arc<DiffDoc>>,
}

impl DiffSelection {
    /// An empty selection.
    pub fn new(_cx: &mut App) -> Self {
        Self::default()
    }

    /// A new document (or a mode switch): clears the selection and remembers the document for copying.
    pub fn reset(&mut self, doc: &Arc<DiffDoc>, _cx: &mut App) {
        self.doc = Some(doc.clone());
    }

    /// Wraps a row's text cell so it takes part in selection. Stub: returns `text` unchanged.
    pub fn text_cell(
        &self,
        _key: SelKey,
        text: StyledText,
        _window: &mut Window,
        _cx: &mut App,
    ) -> AnyElement {
        text.into_any_element()
    }

    /// Whether any text is selected.
    pub fn has_selection(&self, _cx: &App) -> bool {
        false
    }

    /// The source text of the selection: full lines, joined with '\n'.
    pub fn selected_text(&self, _cx: &App) -> Option<String> {
        None
    }

    /// Clears the selection.
    pub fn clear(&mut self, _cx: &mut App) {}

    /// The document the selection belongs to.
    pub fn doc(&self) -> Option<&Arc<DiffDoc>> {
        self.doc.as_ref()
    }
}

/// Registers the selection's key bindings (`secondary-c` / `secondary-a` in the `Diff` context, G7b).
pub fn register(_cx: &mut App) {}
