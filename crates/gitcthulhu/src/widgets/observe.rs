//! `observed(id)`: registers an element for headless UI queries (spec 21 §8.1).
//!
//! `.id(id).test_support()` returns an element that is no longer `Styled`/`ParentElement`, so it has to be the last
//! call of a builder chain. [`observed`] is a styled box that applies it on conversion, which lets a region write
//! `observed(ids::TOOLBAR).h(dp(64.)).child(..)`. Without the kit's `test-support` feature the observation is
//! free.

use gpui_kit::{
    AnyElement, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    StyleRefinement, Styled, TestSupportExt as _, div,
};

/// A `div` that is registered under `id` for [`crate::test_support`] queries when rendered.
pub struct ObservedDiv {
    id: ElementId,
    inner: Div,
}

/// A styled box observable under `id`. Ids must be unique within a frame.
pub fn observed(id: impl Into<ElementId>) -> ObservedDiv {
    ObservedDiv {
        id: id.into(),
        inner: div(),
    }
}

/// Registers an existing `div` under `id` (see [`observed`] for the builder form).
pub trait ObservedExt {
    /// Observes this element under `id`.
    fn observed(self, id: impl Into<ElementId>) -> AnyElement;
}

impl ObservedExt for Div {
    fn observed(self, id: impl Into<ElementId>) -> AnyElement {
        self.id(id).test_support().into_any_element()
    }
}

impl Styled for ObservedDiv {
    fn style(&mut self) -> &mut StyleRefinement {
        self.inner.style()
    }
}

impl ParentElement for ObservedDiv {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.inner.extend(elements);
    }
}

impl IntoElement for ObservedDiv {
    type Element = <AnyElement as IntoElement>::Element;

    fn into_element(self) -> Self::Element {
        self.inner
            .id(self.id)
            .test_support()
            .into_any_element()
            .into_element()
    }
}
