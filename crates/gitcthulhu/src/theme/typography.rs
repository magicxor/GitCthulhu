//! Text styles (spec 21 §3.4): Gitnuro's typography on Inter and Geist Mono.
//!
//! GPUI has no letter spacing, so Gitnuro's tracking is not reproduced. GPUI's default line height is 1.618x, so
//! every helper sets the line height explicitly. Colour is never part of a [`TextKind`]; callers set
//! `text_color(tokens.on_background)`.

use gpui_kit::{FontWeight, Styled, relative};

use crate::fonts::{FONT_MONO, FONT_UI};
use crate::metrics::dp;

/// Line height of Inter relative to its size (07 §0: ascender 1984, descender 494, 2048 units per em).
pub const LINE_HEIGHT_UI: f32 = 1.21;
/// Line height of Geist Mono relative to its size (1005 + 295 over 1000 units per em).
pub const LINE_HEIGHT_MONO: f32 = 1.30;

/// The text styles of the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKind {
    H1,
    H2,
    H3,
    H4,
    Body1,
    Body2,
    Caption,
    Tooltip,
    Button,
    Mono13,
    Mono14,
    MonoCaption,
}

/// Concrete values of a [`TextKind`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSpec {
    /// Font size in dp.
    pub size_dp: f32,
    pub weight: FontWeight,
    /// Geist Mono instead of Inter.
    pub mono: bool,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
}

impl TextKind {
    /// The size, weight, family and line height of this kind (07 §13.3).
    pub fn spec(self) -> TextSpec {
        let ui = |size_dp: f32, weight: FontWeight| TextSpec {
            size_dp,
            weight,
            mono: false,
            line_height: LINE_HEIGHT_UI,
        };
        let mono = |size_dp: f32, weight: FontWeight| TextSpec {
            size_dp,
            weight,
            mono: true,
            line_height: LINE_HEIGHT_MONO,
        };
        match self {
            TextKind::H1 => ui(32., FontWeight::NORMAL),
            TextKind::H2 => ui(24., FontWeight::NORMAL),
            TextKind::H3 => ui(20., FontWeight::NORMAL),
            TextKind::H4 => ui(16., FontWeight::NORMAL),
            TextKind::Body1 => ui(14., FontWeight::NORMAL),
            TextKind::Body2 => ui(13., FontWeight::NORMAL),
            TextKind::Caption => ui(11., FontWeight::NORMAL),
            TextKind::Tooltip => ui(12., FontWeight::NORMAL),
            TextKind::Button => ui(14., FontWeight::MEDIUM),
            TextKind::Mono13 => mono(13., FontWeight::NORMAL),
            TextKind::Mono14 => mono(14., FontWeight::NORMAL),
            TextKind::MonoCaption => mono(11., FontWeight::MEDIUM),
        }
    }
}

/// Text style helpers for any styled element.
pub trait TextStyleExt: Styled + Sized {
    /// Sets family, size (dp), weight and line height of `kind`.
    fn text_kind(self, kind: TextKind) -> Self {
        let spec = kind.spec();
        self.font_family(if spec.mono { FONT_MONO } else { FONT_UI })
            .text_size(dp(spec.size_dp))
            .font_weight(spec.weight)
            .line_height(relative(spec.line_height))
    }

    fn text_h1(self) -> Self {
        self.text_kind(TextKind::H1)
    }
    fn text_h2(self) -> Self {
        self.text_kind(TextKind::H2)
    }
    fn text_h3(self) -> Self {
        self.text_kind(TextKind::H3)
    }
    fn text_h4(self) -> Self {
        self.text_kind(TextKind::H4)
    }
    fn text_body1(self) -> Self {
        self.text_kind(TextKind::Body1)
    }
    fn text_body2(self) -> Self {
        self.text_kind(TextKind::Body2)
    }
    fn text_caption(self) -> Self {
        self.text_kind(TextKind::Caption)
    }
    fn text_tooltip(self) -> Self {
        self.text_kind(TextKind::Tooltip)
    }
    fn text_button(self) -> Self {
        self.text_kind(TextKind::Button)
    }
    fn text_mono13(self) -> Self {
        self.text_kind(TextKind::Mono13)
    }
    fn text_mono14(self) -> Self {
        self.text_kind(TextKind::Mono14)
    }
    fn text_mono_caption(self) -> Self {
        self.text_kind(TextKind::MonoCaption)
    }

    /// Medium weight (500).
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }
    /// SemiBold weight (600).
    fn semibold(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }
    /// Bold maps to SemiBold: there is no Bold font file.
    fn bold(self) -> Self {
        self.font_weight(FontWeight::SEMIBOLD)
    }
    /// Synthetic oblique (there is no italic font file).
    fn italic_text(self) -> Self {
        self.italic()
    }
}

impl<T: Styled> TextStyleExt for T {}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn specs_match_the_typography_table() {
        let h1 = TextKind::H1.spec();
        assert_eq!(
            (h1.size_dp, h1.weight, h1.mono),
            (32., FontWeight::NORMAL, false)
        );
        let button = TextKind::Button.spec();
        assert_eq!(
            (button.size_dp, button.weight),
            (14., FontWeight::MEDIUM),
            "button is 14/500"
        );
        let hint = TextKind::MonoCaption.spec();
        assert!(hint.mono, "keybinding hints are monospaced");
        assert_eq!(
            hint.line_height, LINE_HEIGHT_MONO,
            "mono line height is 1.30"
        );
        assert_eq!(
            TextKind::Body2.spec().line_height,
            LINE_HEIGHT_UI,
            "Inter line height is 1.21"
        );
    }
}
