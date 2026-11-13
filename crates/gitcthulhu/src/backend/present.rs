//! How failures and warnings of operations are shown (spec 21 §2.9, §5.8): the `present` table, the post-hook
//! warning, and the texts of the oplog confirmations and refusals (17 §4.5).

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, SharedString, WeakEntity, Window};

use super::prompt::ConfirmRequest;
use super::{
    ByteSlice as _, ConfirmReason, FullName, GitError, GitErrorKind, ObjectId, OpWarning,
    OplogInProgress, RefDivergence, RefValueInfo, Refusal, RestoreWarning, SkipReason, SkippedFile,
};
use crate::notifications::ToastKind;
use crate::tab::RepoTab;
use crate::tab::ops::{OpKind, OpRetry, RestoreKind};

/// Paths listed in a `WouldOverwrite` dialog before "and N more".
const MAX_LISTED_PATHS: usize = 20;

/// Why an operation did not complete.
#[derive(Clone)]
pub enum OpFailure {
    /// The backend failed (`GitError` is not `Clone`).
    Git(Arc<GitError>),
    /// An interrupted undo must be finished first (from `prepare` or a restore).
    InterruptedRestore,
    /// The repository guard was not free within 2 s (another operation, possibly in another window).
    RepoBusy,
    /// The user declined in `prepare`.
    Cancelled,
    /// Any other failure, as text.
    Other(SharedString),
}

impl OpFailure {
    /// The backend error kind, if this is a backend failure.
    pub fn git_kind(&self) -> Option<GitErrorKind> {
        match self {
            OpFailure::Git(err) => Some(err.kind()),
            _ => None,
        }
    }
}

impl std::fmt::Debug for OpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpFailure::Git(err) => write!(f, "Git({err:?})"),
            OpFailure::InterruptedRestore => f.write_str("InterruptedRestore"),
            OpFailure::RepoBusy => f.write_str("RepoBusy"),
            OpFailure::Cancelled => f.write_str("Cancelled"),
            OpFailure::Other(text) => write!(f, "Other({text:?})"),
        }
    }
}

impl std::fmt::Display for OpFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpFailure::Git(err) => write!(f, "{err}"),
            OpFailure::InterruptedRestore => {
                f.write_str("an interrupted undo must be finished or rolled back first")
            }
            OpFailure::RepoBusy => f.write_str(REPO_BUSY),
            OpFailure::Cancelled => f.write_str("cancelled"),
            OpFailure::Other(text) => f.write_str(text),
        }
    }
}

/// The text of the `RepoBusy` warning (20 §1.5).
pub const REPO_BUSY: &str =
    "Another operation is running in this repository (possibly in another window)";

/// How a failure is shown (one short-lived value per failure, so the large variant is not boxed).
#[allow(clippy::large_enum_variant)]
pub enum ErrorPresentation {
    /// Not at all (cancelled by the user).
    None,
    /// A toast of the operation's tab.
    Toast { kind: ToastKind, text: SharedString },
    /// The error dialog (queued, one at a time).
    Dialog(ErrorDialogData),
    /// A generic recovery flow run by the runner.
    Recovery(RecoveryFlow),
}

/// Content of the error dialog (09 §3.17, D0.6).
pub struct ErrorDialogData {
    pub title: SharedString,
    pub message: SharedString,
    /// Technical details (Git's stderr), behind the details toggle.
    pub details: Option<SharedString>,
    /// Hook output; the output area is expanded by default when `Some`.
    pub hook: Option<HookOutput>,
    /// "Hook failed" warning mode: Warning icon, a single OK.
    pub warning: bool,
    /// For example "Retry without hooks".
    pub retry: Option<RetryAction>,
    /// For example "Open worktree in new tab", "Open Settings", "Set author…".
    pub extra: Option<DialogAction>,
}

impl ErrorDialogData {
    /// A plain error with a title and a message.
    pub fn new(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            details: None,
            hook: None,
            warning: false,
            retry: None,
            extra: None,
        }
    }
}

/// The output of a failed hook.
#[derive(Clone, Debug, PartialEq)]
pub struct HookOutput {
    pub hook: SharedString,
    pub exit_code: Option<i32>,
    pub output: SharedString,
}

/// The retry button of the error dialog: runs `run` on the tab (inside `tab.update`).
pub struct RetryAction {
    pub label: SharedString,
    pub tab: WeakEntity<RepoTab>,
    pub run: OpRetry,
}

/// An extra button of the error dialog.
pub struct DialogAction {
    pub label: SharedString,
    pub run: Box<dyn FnOnce(&mut Window, &mut App)>,
}

/// The generic recovery flows of the runner (spec 21 §2.9).
#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryFlow {
    /// Ask "Trust this repository?" then add it to `safe.directory` and reopen.
    UnsafeRepository { path: PathBuf },
    /// Ask Finish / Roll back for an interrupted undo.
    OplogInterrupted,
}

fn helper_missing_line(cx: &App) -> Option<String> {
    let rt = crate::app::git_runtime(cx)?;
    match rt.helper_status().askpass {
        Ok(_) => None,
        Err(expected) => Some(format!(
            "The credential helper gitcthulhu-askpass is missing: {}",
            expected.display()
        )),
    }
}

fn hook_message(hook: &str, exit_code: Option<i32>) -> String {
    match exit_code {
        Some(code) => format!("The {hook} hook failed (exit code {code})"),
        None => format!("The {hook} hook failed (killed by a signal)"),
    }
}

fn list_paths(paths: &[super::RelaPath]) -> String {
    let mut text: Vec<String> = paths
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|p| p.to_str_lossy().into_owned())
        .collect();
    if paths.len() > MAX_LISTED_PATHS {
        text.push(format!("and {} more", paths.len() - MAX_LISTED_PATHS));
    }
    text.join("\n")
}

/// Decides how a failed operation is shown (first match of the spec 21 §2.9 table wins).
pub fn present(
    kind: OpKind,
    f: &OpFailure,
    retry_without_hooks: Option<OpRetry>,
    tab: WeakEntity<RepoTab>,
    cx: &App,
) -> ErrorPresentation {
    let err = match f {
        OpFailure::Cancelled => return ErrorPresentation::None,
        OpFailure::RepoBusy => {
            return ErrorPresentation::Toast {
                kind: ToastKind::Warning,
                text: REPO_BUSY.into(),
            };
        }
        OpFailure::InterruptedRestore => {
            return ErrorPresentation::Recovery(RecoveryFlow::OplogInterrupted);
        }
        OpFailure::Other(text) => {
            return ErrorPresentation::Dialog(ErrorDialogData::new(
                kind.error_title(),
                text.clone(),
            ));
        }
        OpFailure::Git(err) => err,
    };
    let title = SharedString::from(kind.error_title());
    match &**err {
        GitError::Cancelled | GitError::AuthCancelled { .. } => ErrorPresentation::None,
        GitError::HookFailed {
            hook,
            exit_code,
            output,
        } => {
            let mut data = ErrorDialogData::new(title, hook_message(hook, *exit_code));
            data.hook = Some(HookOutput {
                hook: hook.clone().into(),
                exit_code: *exit_code,
                output: output.clone().into(),
            });
            data.retry = retry_without_hooks.map(|run| RetryAction {
                label: "Retry without hooks".into(),
                tab,
                run,
            });
            ErrorPresentation::Dialog(data)
        }
        GitError::UnsafeRepository { path } => {
            ErrorPresentation::Recovery(RecoveryFlow::UnsafeRepository { path: path.clone() })
        }
        GitError::Conflicts { .. } => ErrorPresentation::Toast {
            kind: ToastKind::Warning,
            text: format!("{} stopped with conflicts", kind.operation_name()).into(),
        },
        GitError::StaleDiff { .. } => ErrorPresentation::Toast {
            kind: ToastKind::Warning,
            text: "The file changed; the diff was refreshed".into(),
        },
        GitError::WouldOverwrite { paths, .. } => {
            let mut data = ErrorDialogData::new(title, format!("{err}:\n{}", list_paths(paths)));
            data.details = err.stderr().map(|s| SharedString::from(s.to_owned()));
            ErrorPresentation::Dialog(data)
        }
        GitError::BranchCheckedOutElsewhere { worktree, .. } => {
            let mut data = ErrorDialogData::new(title, err.to_string());
            data.extra = crate::worktrees::open_in_new_tab_action(worktree.clone());
            ErrorPresentation::Dialog(data)
        }
        GitError::IdentityUnknown { stderr } => {
            let mut data = ErrorDialogData::new(title, err.to_string());
            data.details = Some(stderr.clone().into());
            data.extra = Some(DialogAction {
                label: "Set author…".into(),
                run: Box::new(move |window, cx| {
                    let ctx = tab.upgrade().map(|t| t.read(cx).ctx().clone());
                    crate::dialogs::author::open_author_settings(window, cx, ctx);
                }),
            });
            ErrorPresentation::Dialog(data)
        }
        GitError::AuthFailed { stderr, .. } => {
            let message = match helper_missing_line(cx) {
                Some(line) => format!("{err}\n{line}"),
                None => err.to_string(),
            };
            let mut data = ErrorDialogData::new(title, message);
            data.details = Some(stderr.clone().into());
            ErrorPresentation::Dialog(data)
        }
        GitError::GitNotFound { .. } | GitError::GitTooOld { .. } => {
            let mut data = ErrorDialogData::new(title, err.to_string());
            data.extra = Some(open_settings_action());
            ErrorPresentation::Dialog(data)
        }
        _ => {
            let mut data = ErrorDialogData::new(title, err.to_string());
            data.details = err.stderr().map(|s| SharedString::from(s.to_owned()));
            ErrorPresentation::Dialog(data)
        }
    }
}

/// The "Open Settings" extra button (Environment → Git executable).
pub fn open_settings_action() -> DialogAction {
    DialogAction {
        label: "Open Settings".into(),
        run: Box::new(|window, cx| {
            crate::dialogs::settings::open(
                window,
                cx,
                Some(crate::dialogs::settings::SettingsPage::Environment),
            )
        }),
    }
}

/// The error dialog of a failed repository open (05 §5.2).
pub fn open_failure(err: &GitError) -> ErrorDialogData {
    let mut data = ErrorDialogData::new("Could not open the repository", err.to_string());
    data.details = err.stderr().map(|s| SharedString::from(s.to_owned()));
    if matches!(
        err.kind(),
        GitErrorKind::GitNotFound | GitErrorKind::GitTooOld
    ) {
        data.extra = Some(open_settings_action());
    }
    data
}

/// The warning toast text and the "Show output" dialog of a successful operation with warnings
/// (`OpWarning::PostHookFailed`: "{success text} - the {hook} hook failed (exit code {n})"; `AutostashConflict`:
/// "Your local changes conflicted when re-applied"), plus " (+N more)" for further warnings. `None` when
/// `warnings` holds no warning this version knows.
pub fn post_hook_warning(
    kind: OpKind,
    warnings: &[OpWarning],
) -> Option<(SharedString, ErrorDialogData)> {
    let known: Vec<&OpWarning> = warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                OpWarning::PostHookFailed { .. } | OpWarning::AutostashConflict { .. }
            )
        })
        .collect();
    let first = *known.first()?;
    let more = match known.len() - 1 {
        0 => String::new(),
        n => format!(" (+{n} more)"),
    };
    let (text, data) = match first {
        OpWarning::PostHookFailed {
            hook,
            exit_code,
            output,
        } => {
            let code = match exit_code {
                Some(code) => format!("exit code {code}"),
                None => "killed by a signal".to_owned(),
            };
            let text = format!(
                "{} - the {hook} hook failed ({code})",
                kind.completed_text()
            );
            let mut data = ErrorDialogData::new("Hook failed", hook_message(hook, *exit_code));
            data.hook = Some(HookOutput {
                hook: hook.clone().into(),
                exit_code: *exit_code,
                output: output.clone().into(),
            });
            data.warning = true;
            (text, data)
        }
        OpWarning::AutostashConflict { output } => {
            let text = "Your local changes conflicted when re-applied".to_owned();
            let mut data = ErrorDialogData::new(
                "Your local changes conflicted when re-applied",
                "The operation finished, but re-applying your stashed changes conflicted. They are still in the stash; resolve the conflicts in the working tree.",
            );
            data.details = Some(output.clone().into());
            data.warning = true;
            (text, data)
        }
        _ => return None,
    };
    Some((format!("{text}{more}").into(), data))
}

/// "Some files can't be restored by Undo" confirmation before an operation that would lose untracked files the
/// snapshot cannot hold. Nested Git repositories are listed first and named in the message: their whole history goes.
pub fn confirm_unrestorable(op_title: &str, skipped: &[SkippedFile]) -> ConfirmRequest {
    let is_nested = |s: &&SkippedFile| s.reason == SkipReason::NestedRepository;
    let details = skipped
        .iter()
        .filter(is_nested)
        .chain(skipped.iter().filter(|s| !is_nested(s)))
        .take(MAX_LISTED_PATHS)
        .map(|s| {
            let why = match s.reason {
                SkipReason::TooLarge => "too large",
                SkipReason::OverBudget => "over the capture budget",
                SkipReason::Unreadable => "cannot be read",
                SkipReason::CaptureDisabled => "capturing untracked files is off",
                SkipReason::NestedRepository => "contains a Git repository",
            };
            SharedString::from(format!("{} ({why})", s.path.to_str_lossy()))
        })
        .collect();
    let count = skipped.len();
    let files = if count == 1 { "file" } else { "files" };
    let message = match skipped.iter().filter(is_nested).count() {
        0 => format!("{op_title}: {count} untracked {files} can't be restored by Undo. Continue?"),
        nested => {
            let repositories = if nested == 1 {
                "a Git repository".to_owned()
            } else {
                format!("{nested} Git repositories")
            };
            format!(
                "{op_title}: {count} untracked items can't be restored by Undo, including {repositories} with all \
                 commits, stashes and uncommitted files. Continue?"
            )
        }
    };
    ConfirmRequest {
        title: "Some files can't be restored by Undo".into(),
        message: message.into(),
        details,
        confirm_label: "Continue".into(),
        cancel_label: "Cancel".into(),
        destructive: true,
    }
}

fn short_ref(name: &FullName) -> String {
    name.shorten().to_str_lossy().into_owned()
}

fn short_id(id: &ObjectId) -> String {
    id.to_hex_with_len(7).to_string()
}

fn value_text(value: &Option<RefValueInfo>) -> String {
    match value {
        None => "none".to_owned(),
        Some(RefValueInfo::Object(id)) => short_id(id),
        Some(RefValueInfo::Symbolic(name)) => short_ref(name),
        Some(RefValueInfo::Unborn(name)) => format!("{} (no commits)", short_ref(name)),
    }
}

fn divergence_text(d: &RefDivergence) -> String {
    if d.name.as_bstr() == "HEAD" {
        let from = value_text(&d.expected);
        let to = value_text(&d.actual);
        return format!(
            "You switched from {from} to {to} outside GitCthulhu since the operation. This returns to {from} and keeps only your uncommitted changes."
        );
    }
    format!(
        "{} moved since the operation ({} → {}, outside GitCthulhu).",
        short_ref(&d.name),
        value_text(&d.expected),
        value_text(&d.actual)
    )
}

fn paths_text(paths: &[super::BString]) -> String {
    let mut list: Vec<String> = paths
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|p| p.to_str_lossy().into_owned())
        .collect();
    if paths.len() > MAX_LISTED_PATHS {
        list.push(format!("and {} more", paths.len() - MAX_LISTED_PATHS));
    }
    list.join(", ")
}

/// One line per confirmation reason (17 §4.5).
pub fn confirm_reason_text(reason: &ConfirmReason, redo_restores: bool) -> String {
    let back = if redo_restores {
        " Redo brings it back."
    } else {
        ""
    };
    match reason {
        ConfirmReason::RefsMovedSince(refs) => refs
            .iter()
            .map(divergence_text)
            .collect::<Vec<_>>()
            .join(" ")
            + back,
        ConfirmReason::StagedChangesConflict(paths) => format!(
            "These files were staged after the operation: {}. The restore replaces their staged state.",
            paths_text(paths)
        ),
        ConfirmReason::WorktreeChangesConflict(paths) => format!(
            "These files changed after the operation: {}. The restore replaces them.{}",
            paths_text(paths),
            if redo_restores {
                " Redo brings your changes back."
            } else {
                ""
            }
        ),
        ConfirmReason::PushedSince(refs) => refs
            .iter()
            .map(|r| {
                format!(
                    "{} was pushed to {}/{}. This changes only your local branch; the remote keeps the commits until you force-push.",
                    short_ref(&r.local),
                    r.remote,
                    short_ref(&r.remote_ref)
                )
            })
            .collect::<Vec<_>>()
            .join(" "),
        ConfirmReason::AfterStateUnknown => {
            "The state after the operation was not recorded, so changes made since then may be lost.".to_owned()
        }
        ConfirmReason::ArbitraryRestore { ref_changes } => {
            let mut text = "The repository is restored to the recorded state.".to_owned();
            if !ref_changes.is_empty() {
                let changes: Vec<String> = ref_changes
                    .iter()
                    .map(|d| {
                        format!(
                            "{}: {} → {}",
                            short_ref(&d.name),
                            value_text(&d.actual),
                            value_text(&d.expected)
                        )
                    })
                    .collect();
                text.push_str(" Branch changes: ");
                text.push_str(&changes.join(", "));
                text.push('.');
            }
            text
        }
    }
}

/// The confirmation of an undo, redo or restore that diverged from the recorded state (17 §4.5).
pub fn confirm_restore(kind: &RestoreKind, reasons: &[ConfirmReason]) -> ConfirmRequest {
    let (title, confirm, redo_restores) = match kind {
        RestoreKind::Undo => ("Undo anyway?", "Undo", true),
        RestoreKind::Redo => ("Redo anyway?", "Redo", false),
        RestoreKind::RestoreTo(_) => ("Restore the repository?", "Restore", true),
    };
    let lines: Vec<String> = reasons
        .iter()
        .map(|r| confirm_reason_text(r, redo_restores))
        .collect();
    ConfirmRequest {
        title: title.into(),
        message: lines.join("\n\n").into(),
        details: Vec::new(),
        confirm_label: confirm.into(),
        cancel_label: "Cancel".into(),
        destructive: true,
    }
}

/// A readable name of an in-progress operation ("merge", "interactive rebase").
pub fn in_progress_name(kind: OplogInProgress) -> &'static str {
    match kind {
        OplogInProgress::ApplyMailbox | OplogInProgress::ApplyMailboxRebase => "patch application",
        OplogInProgress::Bisect => "bisect",
        OplogInProgress::CherryPick | OplogInProgress::CherryPickSequence => "cherry-pick",
        OplogInProgress::Merge => "merge",
        OplogInProgress::Rebase => "rebase",
        OplogInProgress::RebaseInteractive => "interactive rebase",
        OplogInProgress::Revert | OplogInProgress::RevertSequence => "revert",
    }
}

/// Why an undo/redo/restore was refused (17 §4.5; cannot be overridden).
/// `action` names the restore that was refused ("Undo", "Redo", "Restore").
pub fn refusal_text(r: &Refusal, action: &str) -> String {
    match r {
        Refusal::OperationInProgress { kind } => format!(
            "A {} is in progress. Finish or abort it first.",
            in_progress_name(*kind)
        ),
        Refusal::InProgressMismatch { expected, actual } => {
            let name = |k: &Option<OplogInProgress>| match k {
                Some(k) => format!("a {} in progress", in_progress_name(*k)),
                None => "no operation in progress".to_owned(),
            };
            format!(
                "The repository has {} but the recorded state has {}.",
                name(actual),
                name(expected)
            )
        }
        Refusal::BranchCheckedOutElsewhere { branch, worktrees } => {
            let places: Vec<String> = worktrees.iter().map(|p| p.display().to_string()).collect();
            format!(
                "This would move '{}', which is checked out in another worktree ({}).",
                short_ref(branch),
                places.join(", ")
            )
        }
        Refusal::MissingObjects(_) => {
            "The commits needed for this are no longer in the repository.".to_owned()
        }
        Refusal::UntrackedInTheWay(paths) => format!(
            "{action} needs to write {}, but an untracked file is in the way. Move it away and try again.",
            paths_text(paths)
        ),
        Refusal::EntryNotFound(_) => "The operation is no longer in the undo history.".to_owned(),
        Refusal::EntryHasNoSnapshot(_) => "This entry has no recorded state to restore.".to_owned(),
        Refusal::UnsupportedEntryVersion(_) => {
            "This entry was recorded by a newer version of GitCthulhu.".to_owned()
        }
    }
}

/// A non-fatal issue of a performed restore, as a toast text.
pub fn restore_warning_text(w: &RestoreWarning) -> String {
    match w {
        RestoreWarning::QuitFailed { kind, stderr } => format!(
            "The {} in progress could not be ended: {}",
            in_progress_name(*kind),
            stderr.trim()
        ),
        RestoreWarning::AutostashSaved { kind } => format!(
            "The stashed changes of the {} were saved as a new stash entry",
            in_progress_name(*kind)
        ),
        RestoreWarning::AfterCaptureFailed { message } => {
            format!("The state after the restore could not be recorded: {message}")
        }
        RestoreWarning::SkippedUntrackedLeftAlone(files) => {
            let n = files.len();
            format!(
                "{n} untracked {} left alone (not captured)",
                if n == 1 { "file was" } else { "files were" }
            )
        }
    }
}
