//! Layout constants of the dialogs (owned by G10; Gitnuro 09 §3). All values are dp. "Content" widths exclude the
//! 16 dp padding of the dialog frame on both sides; the dialog is `content + 2 * DIALOG_PAD` wide.

/// Error dialog content width (09 §3.17).
pub const ERROR_W: f32 = 580.; // 09 §3.17
/// Height of the hook output / details area (09 §3.17).
pub const ERROR_DETAILS_H: f32 = 400.; // 09 §3.17
/// Error icon size (09 §3.17).
pub const ERROR_ICON: f32 = 24.; // 09 §3.17
/// Gap between the message and the details area (09 §3.17).
pub const ERROR_DETAILS_TOP: f32 = 24.; // 09 §3.17
/// Gap between the message and the title (09 §3.17).
pub const ERROR_MESSAGE_TOP: f32 = 16.; // 09 §3.17
/// Gap above the OK row (09 §3.17).
pub const ERROR_BUTTONS_TOP: f32 = 32.; // 09 §3.17
/// Padding of the copy button inside the details area (09 §3.17).
pub const ERROR_COPY_PAD: f32 = 16.; // 09 §3.17

/// Confirmation dialog content width (no Gitnuro counterpart; same family as the choice dialog).
pub const CONFIRM_W: f32 = 420.;
/// Height limit of the detail list of a confirmation.
pub const CONFIRM_DETAILS_MAX_H: f32 = 200.;
/// Choice dialog content width.
pub const CHOICE_W: f32 = 420.;

/// Clone dialog content width (09 §3.1).
pub const CLONE_W: f32 = 720.; // 09 §3.1
/// Horizontal padding of the clone form (09 §3.1).
pub const CLONE_FORM_PAD_H: f32 = 8.; // 09 §3.1
/// Trailing "search" icon button of the directory field (09 §3.1).
pub const CLONE_PICK_BUTTON: f32 = 40.; // 09 §3.1

/// Reset dialog content width (09 §3.6).
pub const RESET_W: f32 = 380.; // 09 §3.6
/// Gap between the reset cards (09 §3.6).
pub const RESET_GAP: f32 = 8.; // 09 §3.6

/// Change-upstream dialog content width and dropdown width (09 §3.7).
pub const UPSTREAM_W: f32 = 400.; // 09 §3.7

/// New / Edit remote dialog content width (09 §3.8).
pub const REMOTE_W: f32 = 600.; // 09 §3.8
/// Gap above the remote dialog's button row (09 §3.8).
pub const REMOTE_BUTTONS_TOP: f32 = 32.; // 09 §3.8

/// New submodule dialog content width (09 §3.9).
pub const SUBMODULE_W: f32 = 480.; // 09 §3.9

/// Author settings: width of one label + field row (09 §3.10).
pub const AUTHOR_ROW_W: f32 = 400.; // 09 §3.10
/// Author settings: label column width including its 16 dp end padding (09 §3.10).
pub const AUTHOR_LABEL_W: f32 = 80.; // 09 §3.10
/// Author settings: horizontal padding of the column (09 §3.10).
pub const AUTHOR_PAD_H: f32 = 8.; // 09 §3.10

/// Commit author identity dialog content width (09 §3.11).
pub const IDENTITY_W: f32 = 400.; // 09 §3.11
/// Sign-off dialog content width (09 §3.12).
pub const SIGNOFF_W: f32 = 300.; // 09 §3.12

/// About dialog size (09 §3.14).
pub const ABOUT_W: f32 = 600.; // 09 §3.14
/// About dialog height (09 §3.14).
pub const ABOUT_H: f32 = 600.; // 09 §3.14

/// Credential dialogs: field width (09 §3.15, §3.16).
pub const CREDENTIAL_W: f32 = 300.; // 09 §3.15
/// Size of the show / hide password toggle (09 §3.15).
pub const EYE_BUTTON: f32 = 20.; // 09 §3.15
/// Width of the host-key dialog content (fingerprints are long).
pub const HOST_KEY_W: f32 = 420.;
