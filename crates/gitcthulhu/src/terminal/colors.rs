//! The terminal's colours from the app's theme tokens (21 §3.1): ground and text, cursor, selection and the 16 ANSI
//! colours, so programs in the terminal match the theme (adapted from the old view's `with_own_terminal`, 18 §5.2).

use but_tty::{Colors, RgbColor};
use gpui_kit::{Hsla, Rgba};

use crate::theme::Tokens;

/// What a terminal is drawn in: the palette and defaults, and the ground and text for what they leave unset.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TerminalColors {
    pub colors: Colors,
    pub background: RgbColor,
    pub foreground: RgbColor,
}

impl TerminalColors {
    /// The colours of the theme with `tokens` and commit-graph `lanes` (whose blue and pink stand in for ANSI blue
    /// and magenta, which Gitnuro's tokens lack).
    pub fn of(tokens: &Tokens, lanes: &[Hsla; 6]) -> Self {
        let ground = tokens.background;
        let flat = |color: Hsla| over(color, ground);
        let bg = rgb(ground);
        let fg = rgb(flat(tokens.on_background));
        let secondary = rgb(flat(tokens.on_background_secondary));
        let black = if tokens.is_light {
            fg
        } else {
            rgb(flat(tokens.background_selected))
        };
        let accents = [
            flat(tokens.error),
            flat(tokens.add_file),
            flat(tokens.conflicting_file),
            flat(lanes[0]),
            flat(lanes[5]),
            flat(tokens.diff_keyword),
        ]
        .map(rgb);
        let normal = std::iter::once(black)
            .chain(accents)
            .chain(std::iter::once(secondary));
        let bright = std::iter::once(mix(secondary, bg, 0.35))
            .chain(accents.map(|accent| mix(accent, fg, 0.25)))
            .chain(std::iter::once(fg));
        let selection = rgb(over(tokens.primary.alpha(0.45), ground));
        TerminalColors {
            colors: Colors {
                background: Some(bg),
                foreground: Some(fg),
                cursor: Some(fg),
                selection_background: Some(selection),
                selection_foreground: None,
                palette: (0u8..).zip(normal.chain(bright)).collect(),
            },
            background: bg,
            foreground: fg,
        }
    }
}

/// `color` drawn over `ground`, opaque.
fn over(color: Hsla, ground: Hsla) -> Hsla {
    let (top, bottom) = (Rgba::from(color), Rgba::from(ground));
    let a = top.a;
    Rgba {
        r: top.r * a + bottom.r * (1. - a),
        g: top.g * a + bottom.g * (1. - a),
        b: top.b * a + bottom.b * (1. - a),
        a: 1.,
    }
    .into()
}

/// `from` moved `amount` (0..1) of the way toward `to`.
fn mix(from: RgbColor, to: RgbColor, amount: f32) -> RgbColor {
    let channel = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount)
            .round()
            .clamp(0., 255.) as u8
    };
    RgbColor {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
    }
}

/// An opaque GPUI colour as the terminal's.
pub(super) fn rgb(color: Hsla) -> RgbColor {
    let rgba = Rgba::from(color);
    let byte = |channel: f32| (channel * 255.).round().clamp(0., 255.) as u8;
    RgbColor {
        r: byte(rgba.r),
        g: byte(rgba.g),
        b: byte(rgba.b),
    }
}

/// A terminal colour as GPUI's.
pub(super) fn hsla(color: RgbColor) -> Hsla {
    Rgba {
        r: f32::from(color.r) / 255.,
        g: f32::from(color.g) / 255.,
        b: f32::from(color.b) / 255.,
        a: 1.,
    }
    .into()
}
