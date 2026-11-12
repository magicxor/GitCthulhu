//! The reusable widgets of the GitCthulhu look (Gitnuro's shared components, spec 21 §1.1, §4.6).
//!
//! Every widget takes its colours from [`crate::theme::Tokens`] and its sizes from [`crate::metrics`], so it
//! follows the theme and scale settings. Widgets that expose an element id render with `.test_support()` so tests
//! can query their bounds.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{Bounds, Hsla, IntoElement, Pixels, Styled as _, Window, canvas};

pub mod avatar;
pub mod buttons;
pub mod checkbox;
pub mod date;
pub mod dropdown;
pub mod expandable;
pub mod field;
pub mod file_row;
pub mod file_tree;
pub mod header_bar;
pub mod observe;
pub mod progress;
pub mod radio;
pub mod scroll;
pub mod split;
pub mod switch;
pub mod tooltip;
pub mod v_resizer;

pub use buttons::{
    ButtonTile, ExtendedMenuButton, IconButton, IconTextButton, MenuButton, PrimaryButton,
    SecondaryButton, SecondaryButtonCompactable, StateIcon, TextLink,
};
pub use checkbox::CheckboxText;
pub use dropdown::{DropDownMenuText, FilterDropdown};
pub use expandable::VerticalExpandable;
pub use field::{FilledTextArea, OutlinedField, SearchField};
pub use header_bar::HeaderBar34;
pub use observe::{ObservedDiv, ObservedExt, observed};
pub use progress::{CircularProgress, LinearProgress};
pub use radio::RadioRow;
pub use split::{SplitGutter, ThreePaneSplit};
pub use switch::AppSwitch;
pub use tooltip::{DelayedTooltip, InstantTooltip, KeybindingHint, TooltipExt, TooltipText};
pub use v_resizer::VResizer;

#[cfg(test)]
mod tests;

/// Callback of a clickable widget.
pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

use gpui_kit::App;

/// `base` with the Material hover layer on top (see [`crate::theme::Tokens::hover_layer`]).
pub fn with_layer(base: Hsla, layer: Hsla) -> Hsla {
    base.blend(layer)
}

/// Shared cell that [`track_bounds`] fills with the layout bounds of its parent.
pub type BoundsCell = Rc<Cell<Bounds<Pixels>>>;

/// An invisible element that records the bounds of the `relative()` box it is placed in. Dropdowns read the cell
/// on click to anchor their popup at the trigger's bottom-left corner.
///
/// An absolutely positioned child covers its parent's *padding* box, so the parent must have no border (and the
/// recorded box then is the parent's border box). Put the tracker in an unstyled wrapper around a bordered trigger.
pub fn track_bounds(cell: &BoundsCell) -> impl IntoElement {
    let cell = cell.clone();
    canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}
