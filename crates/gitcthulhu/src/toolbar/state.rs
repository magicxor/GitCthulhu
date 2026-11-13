//! What the toolbar buttons show: enabled states, tooltips and keybinding hints (07 §5.1.1).

use crate::settings::Settings;
use crate::tab::model::RepoModel;
use crate::widgets::KeybindingHint;

/// The conditions the toolbar's buttons depend on, read from the model and the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolbarState {
    /// The repository has at least one remote (Pull and Push are enabled).
    pub has_remote: bool,
    /// Something is staged or unstaged (Stash is enabled).
    pub can_stash: bool,
    /// At least one stash exists (Pop stash is enabled).
    pub has_stash: bool,
    /// "Pull with rebase as default".
    pub pull_with_rebase: bool,
}

impl ToolbarState {
    /// The state for `model` (a model that is not loaded yet disables everything that needs data).
    pub fn read(model: &RepoModel, settings: &Settings) -> Self {
        let refs = model.refs.value.as_ref();
        Self {
            has_remote: refs.is_some_and(|r| !r.remotes.is_empty()),
            can_stash: model
                .status
                .value
                .as_ref()
                .is_some_and(|s| !s.counts.is_clean()),
            has_stash: refs.is_some_and(|r| !r.stashes.is_empty()),
            pull_with_rebase: settings.pull_with_rebase,
        }
    }

    /// The tooltip of Pull.
    pub fn pull_tooltip(&self) -> &'static str {
        match (self.has_remote, self.pull_with_rebase) {
            (false, _) => "Pull not available without a remote repository",
            (true, true) => "Pull current branch with rebase",
            (true, false) => "Pull current branch",
        }
    }

    /// The tooltip of Push.
    pub fn push_tooltip(&self) -> &'static str {
        if self.has_remote {
            "Push current branch changes"
        } else {
            "Push not available without a remote repository"
        }
    }

    /// The tooltip of Stash.
    pub fn stash_tooltip(&self) -> &'static str {
        if self.can_stash {
            "Stash uncommitted changes"
        } else {
            "Change some files to be able to stash them"
        }
    }

    /// The tooltip of Pop stash.
    pub fn pop_tooltip(&self) -> &'static str {
        if self.has_stash {
            "Pop the last stash"
        } else {
            "No stashes to pop"
        }
    }
}

/// The hint of a shortcut on the platform's primary modifier (Ctrl, or the command key on macOS), e.g.
/// `Ctrl+Shift+S`.
pub fn primary_hint(alt: bool, shift: bool, key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    KeybindingHint::text_of(!mac, mac, alt, shift, key)
}
