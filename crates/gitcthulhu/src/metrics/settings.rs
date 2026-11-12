//! Layout constants of the settings dialog (owned by G11; 09 §3.18). All values are dp.

/// Dialog width (the frame draws no padding of its own).
pub const DIALOG_W: f32 = 1000.;
/// Dialog height; the dialog never exceeds the window.
pub const DIALOG_H: f32 = 720.;
/// Width of the category column.
pub const LEFT_W: f32 = 200.;
/// Padding around the "Settings" title.
pub const TITLE_PAD: f32 = 16.;
/// Height of a category entry (24 dp icon plus 4 dp above and below).
pub const ENTRY_H: f32 = 32.;
/// Category icon size.
pub const ENTRY_ICON: f32 = 24.;
/// Space before a section header (except the first).
pub const SECTION_GAP: f32 = 16.;
/// Horizontal padding of section headers and the start padding of entry icons.
pub const SECTION_PAD_H: f32 = 16.;
/// Vertical padding of section headers.
pub const SECTION_PAD_V: f32 = 4.;
/// Gap between an entry's icon and its text (the icon's end padding).
pub const ENTRY_ICON_GAP: f32 = 8.;
/// Top padding of the content column.
pub const CONTENT_TOP: f32 = 64.;
/// Start and end padding of the content column, and the Accept button's end and bottom padding.
pub const CONTENT_PAD_H: f32 = 16.;
/// Vertical padding of a setting row.
pub const ROW_PAD_V: f32 = 8.;
/// Space between a row's title and its subtitle.
pub const SUBTITLE_GAP: f32 = 4.;
/// Minimum space between a row's titles and its control.
pub const CONTROL_GAP: f32 = 16.;
/// Width of a dropdown control.
pub const DROPDOWN_W: f32 = 180.;
/// Width of a text field control.
pub const TEXT_FIELD_W: f32 = 240.;
/// Width of a number field control.
pub const NUMBER_FIELD_W: f32 = 136.;
/// Height of the custom theme editor.
pub const THEME_EDITOR_H: f32 = 120.;
/// How long an invalid number flashes the error border, in milliseconds.
pub const FLASH_MS: u64 = 500;
/// Quiet period before the custom theme text is applied, in milliseconds.
pub const THEME_DEBOUNCE_MS: u64 = 1000;
/// Quiet period before changed Git settings rebuild the runtime, in milliseconds.
pub const RUNTIME_DEBOUNCE_MS: u64 = 600;
/// Quiet period before the proxy password is written to the keychain, in milliseconds.
pub const PASSWORD_DEBOUNCE_MS: u64 = 500;
/// Alpha of the titles of a disabled row.
pub const DISABLED_ALPHA: f32 = 0.6;
