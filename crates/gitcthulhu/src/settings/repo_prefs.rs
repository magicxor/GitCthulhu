//! `repos.json`: per-repository preferences keyed by canonical repository path (spec 21 §7.4).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::io::{Loaded, load, write_atomic};
use super::state::canonical;

/// Sign-off configuration of one repository (replaces Gitnuro's per-repo INI).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Signoff {
    pub enabled: bool,
    pub text: String,
}

/// Preferences of one repository.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoPref {
    pub signoff: Signoff,
    /// Forge override (host name or kind) when detection from remotes is wrong.
    pub forge_override: Option<String>,
    /// Preferred forge account (opaque; owned by the services crate).
    pub preferred_forge_user: Option<serde_json::Value>,
    /// Path of the pull request template.
    pub pr_template_path: Option<String>,
}

/// All repository preferences.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoPrefs {
    map: BTreeMap<String, RepoPref>,
}

fn key_of(repo: &Path) -> String {
    canonical(repo).to_string_lossy().into_owned()
}

impl RepoPrefs {
    /// Loads `repos.json` from `path`: empty when missing, empty plus a backup when corrupt (see [`load`]).
    pub fn load_from(path: &Path) -> Loaded<RepoPrefs> {
        load::<RepoPrefs>(path)
    }

    /// Writes the preferences to `path` atomically.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, self)
    }

    /// The preferences of `repo` (defaults when none were stored).
    pub fn get(&self, repo: &Path) -> RepoPref {
        self.map.get(&key_of(repo)).cloned().unwrap_or_default()
    }

    /// Edits the preferences of `repo`, creating the entry if needed. Entries equal to the defaults are dropped so
    /// the file does not grow with untouched repositories.
    pub fn edit(&mut self, repo: &Path, edit: impl FnOnce(&mut RepoPref)) {
        let key = key_of(repo);
        let mut pref = self.map.remove(&key).unwrap_or_default();
        edit(&mut pref);
        if pref != RepoPref::default() {
            self.map.insert(key, pref);
        }
    }

    /// Number of repositories with stored preferences.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// `true` when no repository has stored preferences.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
