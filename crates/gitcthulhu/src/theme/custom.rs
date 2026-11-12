//! Custom theme JSON (owned by G11 after G0b; spec 21 §3, 07 §13.1): Gitnuro's 28 colour keys plus `isLight`,
//! ARGB hex strings. Parsing is `theme::parse_custom`; G11 adds the export and the settings UI.

use super::{CustomThemeError, Tokens, parse_custom};

/// Parses a custom theme JSON (missing colour keys take the Dark value).
pub fn parse(json: &str) -> Result<Tokens, CustomThemeError> {
    parse_custom(json)
}
