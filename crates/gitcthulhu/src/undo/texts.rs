//! Texts and small pure helpers of the undo UI: button tooltips (17 §4.5), history badges, relative times and the
//! icon of an operation kind. Nothing here touches GPUI state, so every rule is unit-tested directly.

use gpui_kit::SharedString;

use crate::backend::present::in_progress_name;
use crate::backend::{
    EntryRole, EntryStatus, GitTime, OperationKind, OplogEntry, StackItemInfo, UndoRedoState,
};
use crate::icons::AppIcon;
use crate::widgets::KeybindingHint;
use crate::widgets::date::{DateFormat, format_date, local_offset_minutes};

/// Tooltip of a disabled button while an interrupted undo waits for recovery (17 §4.5).
pub const INTERRUPTED_TIP: &str = "Finish or roll back the interrupted undo first";
/// Tooltip of the disabled Undo button.
pub const NOTHING_TO_UNDO: &str = "Nothing to undo";
/// Tooltip of the disabled Redo button.
pub const NOTHING_TO_REDO: &str = "Nothing to redo";

/// What a toolbar button shows: whether it reacts, its tooltip and the keybinding hint of the tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonTip {
    /// The button reacts to clicks.
    pub enabled: bool,
    /// Tooltip text (the wording of 17 §4.5 for this state).
    pub text: SharedString,
    /// `Ctrl+Z` / `Ctrl+Shift+Z`, shown while the button is enabled.
    pub hint: Option<SharedString>,
}

/// Which of the two buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// The Undo button.
    Undo,
    /// The Redo button.
    Redo,
}

/// The shortcut hint of a button: the platform's primary modifier plus `Z` (`Shift` for Redo).
pub fn hint_of(which: Which) -> SharedString {
    let mac = cfg!(target_os = "macos");
    KeybindingHint::text_of(!mac, mac, false, which == Which::Redo, "Z").into()
}

/// The state and tooltip of one button.
///
/// - `state`: the model's `UndoRedoState` (`None` when the oplog is unavailable or not loaded yet);
/// - `unavailable`: why undo is unavailable, when it is;
/// - `busy`: an operation of the tab runs.
pub fn button_tip(
    which: Which,
    state: Option<&UndoRedoState>,
    unavailable: Option<&SharedString>,
    busy: bool,
) -> ButtonTip {
    let disabled = |text: SharedString| ButtonTip {
        enabled: false,
        text,
        hint: None,
    };
    let Some(state) = state else {
        return match unavailable {
            Some(reason) => disabled(unavailable_text(reason)),
            None => disabled(nothing_text(which).into()),
        };
    };
    if state.interrupted_restore {
        return disabled(INTERRUPTED_TIP.into());
    }
    let item = match which {
        Which::Undo => state.undo.as_ref(),
        Which::Redo => state.redo.as_ref(),
    };
    let Some(item) = item else {
        return disabled(nothing_text(which).into());
    };
    ButtonTip {
        enabled: !busy,
        text: item_text(which, item).into(),
        hint: (!busy).then(|| hint_of(which)),
    }
}

fn nothing_text(which: Which) -> &'static str {
    match which {
        Which::Undo => NOTHING_TO_UNDO,
        Which::Redo => NOTHING_TO_REDO,
    }
}

/// `Undo is not available for this repository: {reason}` (a reason that already has the sentence keeps it).
pub fn unavailable_text(reason: &SharedString) -> SharedString {
    if reason.starts_with("Undo is not available") {
        reason.clone()
    } else {
        format!("Undo is not available for this repository: {reason}").into()
    }
}

/// `Undo Commit "Fix login"`, with the in-progress suffix of 17 §4.5 when the step also ends an operation.
fn item_text(which: Which, item: &StackItemInfo) -> String {
    let verb = match which {
        Which::Undo => "Undo",
        Which::Redo => "Redo",
    };
    let mut text = format!("{verb} {}", item.title);
    if let Some(kind) = item.ends_in_progress {
        text.push_str(&format!(
            " \u{2014} also ends the {} in progress",
            in_progress_name(kind)
        ));
        if which == Which::Undo {
            text.push_str(" (can't be redone)");
        }
    }
    text
}

/// The icon of an operation kind in the history list.
pub fn kind_icon(kind: OperationKind) -> AppIcon {
    use OperationKind::*;
    match kind {
        Commit | Amend | ResolveConflicts => AppIcon::Done,
        Revert | CherryPick | ResetSoft | ResetMixed | ResetHard => AppIcon::Revert,
        Merge
        | Rebase
        | RebaseInteractive
        | Checkout
        | CheckoutCommit
        | CheckoutRemoteBranch
        | CreateBranch
        | CreateBranchAndCheckout
        | DeleteBranch
        | RenameBranch => AppIcon::Branch,
        Pull => AppIcon::Download,
        Push | PushTags | DeleteRemoteBranch | DeleteRemoteTag => AppIcon::Upload,
        CreateTag | DeleteTag => AppIcon::Tag,
        StashPush | StashDrop | StashClear => AppIcon::Stash,
        StashApply | StashPop => AppIcon::ApplyStash,
        Discard | DeleteUntracked => AppIcon::Delete,
        ContinueOperation | SkipOperation | AbortOperation => AppIcon::Start,
        Undo | Redo | RestoreTo => AppIcon::Undo,
        Barrier => AppIcon::Info,
        _ => AppIcon::History,
    }
}

/// The status badges of a history row (17 §4.5 / 21 §5.1): `Undone`, `Not undoable`, `Pushed` for markers and
/// `Some files not captured` when untracked files were not part of the snapshot.
pub fn badges(entry: &OplogEntry) -> Vec<&'static str> {
    let mut badges = Vec::new();
    match entry.status {
        EntryStatus::Undone { .. } | EntryStatus::Superseded => badges.push("Undone"),
        EntryStatus::NotUndoable => badges.push("Not undoable"),
        EntryStatus::Marker => badges.push("Pushed"),
        _ => {}
    }
    if !entry.skipped_untracked.is_empty() {
        badges.push("Some files not captured");
    }
    badges
}

/// Whether `entry` has recorded states at all: markers and barriers do not, so their rows have no restore menu.
pub fn has_snapshot(entry: &OplogEntry) -> bool {
    !matches!(entry.role, EntryRole::Marker | EntryRole::Barrier { .. })
}

/// Whether "Restore to before/after this operation" is enabled for `entry` on the given side: an entry of a newer
/// format cannot be restored, and an entry whose after-state failed to be captured has no "after".
pub fn can_restore(entry: &OplogEntry, after: bool) -> bool {
    has_snapshot(entry) && entry.restorable && (!after || entry.after_captured)
}

/// A short relative time for the history list: `just now`, `5 min ago`, `3 h ago`, then the formatted date with
/// the time.
pub fn relative_time(finished_ms: i64, now_ms: i64, format: &DateFormat) -> String {
    let seconds = (now_ms - finished_ms).div_euclid(1000);
    match seconds {
        ..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => {
            let at = finished_ms.div_euclid(1000);
            let now = now_ms.div_euclid(1000);
            let offset = local_offset_minutes(at);
            format_date(
                GitTime {
                    seconds: at,
                    offset_minutes: offset,
                },
                GitTime {
                    seconds: now,
                    offset_minutes: offset,
                },
                offset,
                format,
                true,
            )
        }
    }
}

/// The current time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
