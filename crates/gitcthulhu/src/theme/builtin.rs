//! The built-in Dark and Light token values (Gitnuro `theme/Color.kt`, 07 §13.1 verbatim), the commit-graph lane
//! palette and the remote chip colour.

use gpui_kit::Hsla;

use super::{Tokens, argb_to_hsla};

/// Dark theme, ARGB `0xAARRGGBB`, keyed by Gitnuro's camelCase token names (the custom theme JSON keys).
pub const DARK_ARGB: &[(&str, u32)] = &[
    ("primary", 0xFF0169CB),
    ("primaryVariant", 0xFF9FD1FF),
    ("onPrimary", 0xFFFFFFFF),
    ("secondary", 0xFFE9C754),
    ("onSecondary", 0xFF0E1621),
    ("onBackground", 0xFFFFFFFF),
    ("onBackgroundSecondary", 0xFFCCCBCB),
    ("error", 0xFFC93838),
    ("onError", 0xFFFFFFFF),
    ("background", 0xFF0F0F0F),
    ("backgroundSelected", 0xFF2F3640),
    ("surface", 0xFF1F1F22),
    ("secondarySurface", 0xFF1A2B3D),
    ("tertiarySurface", 0xFF0A335C),
    ("addFile", 0xFF32A852),
    ("deletedFile", 0xFFC93838),
    ("modifiedFile", 0xFF0070D8),
    ("conflictingFile", 0xFFFFB638),
    ("dialogOverlay", 0xAA000000),
    ("normalScrollbar", 0xFF888888),
    ("hoverScrollbar", 0xFFCCCCCC),
    ("diffLineAdded", 0xAA566F5A),
    ("diffContentAdded", 0x4500FF00),
    ("diffLineRemoved", 0xAA6F585E),
    ("diffContentRemoved", 0x45FF0000),
    ("diffKeyword", 0xFFA3CAF1),
    ("diffAnnotation", 0xFFB3AE5F),
    ("diffComment", 0xFF70C290),
];

/// Light theme, ARGB. `onPrimary` is `FFFFFFFF`: the Gitnuro source literal has nine hex digits and Compose masks
/// it to the low 32 bits (07 §13.1).
pub const LIGHT_ARGB: &[(&str, u32)] = &[
    ("primary", 0xFF0070D8),
    ("primaryVariant", 0xFF0070D8),
    ("onPrimary", 0xFFFFFFFF),
    ("secondary", 0xFF9C27B0),
    ("onSecondary", 0xFFFFFFFF),
    ("onBackground", 0xFF212934),
    ("onBackgroundSecondary", 0xFF595858),
    ("error", 0xFFC93838),
    ("onError", 0xFFFFFFFF),
    ("background", 0xFFFFFFFF),
    ("backgroundSelected", 0xC0CEE1F2),
    ("surface", 0xFFF8F8F8),
    ("secondarySurface", 0xFFE9ECEF),
    ("tertiarySurface", 0xFFE9ECEF),
    ("addFile", 0xFF32A852),
    ("deletedFile", 0xFFC93838),
    ("modifiedFile", 0xFF0070D8),
    ("conflictingFile", 0xFFFFB638),
    ("dialogOverlay", 0xAA000000),
    ("normalScrollbar", 0xFFCCCCCC),
    ("hoverScrollbar", 0xFF0070D8),
    ("diffLineAdded", 0xAAD7EBD0),
    ("diffContentAdded", 0x4570DF70),
    ("diffLineRemoved", 0xAAF0D4D4),
    ("diffContentRemoved", 0x45FF0000),
    ("diffKeyword", 0xFF3B83CF),
    ("diffAnnotation", 0xFF9F9927),
    ("diffComment", 0xFF0D9141),
];

/// The Dark theme tokens.
pub fn dark() -> Tokens {
    Tokens::from_argb(DARK_ARGB, false)
}

/// The Light theme tokens.
pub fn light() -> Tokens {
    Tokens::from_argb(LIGHT_ARGB, true)
}

/// Commit-graph lane colours (08 §0.1 / §2.9): `#42A5F5 #EF5350 #78909C #FF7043 #66BB6A #EC407A`.
pub fn lanes() -> [Hsla; 6] {
    [
        0xFF42A5F5, 0xFFEF5350, 0xFF78909C, 0xFFFF7043, 0xFF66BB6A, 0xFFEC407A,
    ]
    .map(argb_to_hsla)
}

/// Remote branch chip colour (08 §2.12): `#BD9B46`.
pub fn remote_chip() -> Hsla {
    argb_to_hsla(0xFFBD9B46)
}
