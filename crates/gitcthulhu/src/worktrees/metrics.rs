//! Layout constants of the Worktrees section and the Add worktree dialog (spec 21 §5.7). All values are dp.

/// Size of the lock and prunable markers in a row (spec 21 §5.7: `Lock` 14 dp).
pub const MARKER_ICON: f32 = 14.;
/// Space between the markers and the branch text of a row.
pub const MARKER_GAP: f32 = 4.;

/// Add worktree dialog: content width (the frame adds 16 dp on both sides, 560 dp in total; spec 21 §5.7).
pub const DIALOG_W: f32 = 528.;
/// Add worktree dialog: estimated height for the dialog host.
pub const DIALOG_EST_H: f32 = 620.;
/// Gap above a form label.
pub const LABEL_TOP: f32 = 16.;
/// Gap between a form label and its control.
pub const LABEL_BOTTOM: f32 = 8.;
/// Gap between the target cards.
pub const CARD_GAP: f32 = 8.;
/// Width of the "Browse" icon button's end inset inside the path field.
pub const BROWSE_END: f32 = 24.;
/// End padding the path field reserves for the overlaid "Browse" button (as the Clone dialog's folder button,
/// 09 §3.1), so a long path never runs under it.
pub const BROWSE_BUTTON: f32 = 40.;
