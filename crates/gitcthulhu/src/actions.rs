//! Window- and repository-level actions and their key bindings (spec 21 §2.12). Region packages define and bind their
//! own actions in their `register(cx)` (`registry.rs`); they never bind keys in the `AppView` or `RepoView`
//! contexts.

use gpui_kit::{App, KeyBinding, NoAction};

/// Key context of the window content (AppView).
pub const APP_CONTEXT: &str = "AppView";
/// Key context of the repository frame (RepoView).
pub const REPO_CONTEXT: &str = "RepoView";
/// Key context of the processing overlay; it swallows every ancestor key (07 §11).
pub const PROCESSING_CONTEXT: &str = "Processing";

gpui_kit::actions!(
    gitcthulhu,
    [
        /// New empty tab.
        NewTab,
        /// Close the current tab.
        CloseTab,
        /// Previous tab (no wrap).
        PrevTab,
        /// Next tab (no wrap).
        NextTab,
        /// Welcome: directory picker; repository: Open popup.
        OpenRepository,
        /// The Settings dialog.
        OpenSettings,
        /// Pull (default type).
        Pull,
        /// Push the current branch.
        Push,
        /// The New-branch dialog.
        CreateBranch,
        /// Stash all changes.
        Stash,
        /// Pop the newest stash.
        PopStash,
        /// Reload everything (unless an operation runs).
        Refresh,
        /// Close the most recently opened closeable view.
        CloseLastView,
        /// Undo the last operation (D0.3; milestone 1: no-op slot).
        Undo,
        /// Redo (D0.3; milestone 1: no-op slot).
        Redo,
        /// Show/hide the embedded terminal (D0.4; milestone 1: no-op slot).
        ToggleTerminal,
        /// Escape on the processing overlay: cancel a cancellable operation.
        CancelOperation,
    ]
);

/// The `RepoView && !Input` predicate of Undo/Redo: text inputs keep their own undo.
const REPO_NOT_INPUT: &str = "RepoView && !Input";

/// Every G0 binding as (keystroke, context). Windows/Linux and macOS spellings (`secondary-` = Ctrl / Cmd). The
/// terminal (X1) and the processing overlay bind `NoAction` for each entry so these keys reach them.
pub const ANCESTOR_KEYS: &[(&str, &str)] = &[
    ("secondary-t", APP_CONTEXT),
    ("secondary-w", APP_CONTEXT),
    ("alt-left", APP_CONTEXT),
    ("ctrl-shift-tab", APP_CONTEXT),
    ("alt-right", APP_CONTEXT),
    ("ctrl-tab", APP_CONTEXT),
    ("secondary-o", APP_CONTEXT),
    ("secondary-alt-s", APP_CONTEXT),
    ("secondary-u", REPO_CONTEXT),
    ("secondary-p", REPO_CONTEXT),
    ("secondary-b", REPO_CONTEXT),
    ("secondary-s", REPO_CONTEXT),
    ("secondary-shift-s", REPO_CONTEXT),
    ("f5", REPO_CONTEXT),
    ("secondary-r", REPO_CONTEXT),
    ("escape", REPO_CONTEXT),
    ("secondary-z", REPO_NOT_INPUT),
    ("secondary-shift-z", REPO_NOT_INPUT),
    ("ctrl-y", REPO_NOT_INPUT),
    ("ctrl-`", REPO_CONTEXT),
];

/// Whether `keystroke` is bound on this platform (macOS switches tabs only with Ctrl+Tab / Ctrl+Shift+Tab, 07 §11).
fn bound_on_this_platform(keystroke: &str) -> bool {
    !(cfg!(target_os = "macos") && matches!(keystroke, "alt-left" | "alt-right"))
}

/// The G0 key bindings, one per [`ANCESTOR_KEYS`] entry bound on this platform.
pub fn bindings() -> Vec<KeyBinding> {
    ANCESTOR_KEYS
        .iter()
        .filter(|(key, _)| bound_on_this_platform(key))
        .map(|&(key, context)| {
            let context = Some(context);
            match key {
                "secondary-t" => KeyBinding::new(key, NewTab, context),
                "secondary-w" => KeyBinding::new(key, CloseTab, context),
                "alt-left" | "ctrl-shift-tab" => KeyBinding::new(key, PrevTab, context),
                "alt-right" | "ctrl-tab" => KeyBinding::new(key, NextTab, context),
                "secondary-o" => KeyBinding::new(key, OpenRepository, context),
                "secondary-alt-s" => KeyBinding::new(key, OpenSettings, context),
                "secondary-u" => KeyBinding::new(key, Pull, context),
                "secondary-p" => KeyBinding::new(key, Push, context),
                "secondary-b" => KeyBinding::new(key, CreateBranch, context),
                "secondary-s" => KeyBinding::new(key, Stash, context),
                "secondary-shift-s" => KeyBinding::new(key, PopStash, context),
                "f5" | "secondary-r" => KeyBinding::new(key, Refresh, context),
                "escape" => KeyBinding::new(key, CloseLastView, context),
                "secondary-z" => KeyBinding::new(key, Undo, context),
                "secondary-shift-z" | "ctrl-y" => KeyBinding::new(key, Redo, context),
                "ctrl-`" => KeyBinding::new(key, ToggleTerminal, context),
                other => unreachable!("ANCESTOR_KEYS entry {other} has no action"),
            }
        })
        .collect()
}

/// The processing overlay's bindings: `NoAction` for every ancestor key (it swallows all keys), Escape cancels.
pub fn processing_bindings() -> Vec<KeyBinding> {
    let mut bindings: Vec<KeyBinding> = ANCESTOR_KEYS
        .iter()
        .filter(|(key, _)| *key != "escape")
        .map(|(key, _)| KeyBinding::new(key, NoAction, Some(PROCESSING_CONTEXT)))
        .collect();
    bindings.push(KeyBinding::new(
        "escape",
        CancelOperation,
        Some(PROCESSING_CONTEXT),
    ));
    bindings
}

/// Binds the G0 keys, then lets every region register its own (`registry::register_all`).
pub fn init(cx: &mut App) {
    cx.bind_keys(bindings());
    cx.bind_keys(processing_bindings());
    crate::registry::register_all(cx);
}
