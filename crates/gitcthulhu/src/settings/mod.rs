//! Persistent application settings (spec 21 §7.2) and the debounced persistence glue.
//!
//! [`Settings`] is a serde struct whose defaults are Gitnuro's (plus the GitCthulhu additions). It lives in the
//! [`SettingsGlobal`] global; change it with [`update`], which notifies observers (`cx.observe_global`) and
//! schedules a debounced atomic write of `settings.json`.

use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

use crate::theme::ThemeKind;

pub mod io;
pub mod repo_prefs;
pub mod state;

pub use io::{Loaded, SAVE_DEBOUNCE, backup_path, load, schedule_save, set_base, write_atomic};
pub use repo_prefs::{RepoPref, RepoPrefs, Signoff};
pub use state::{PlacementKind, UiState, UiStateGlobal, WindowState};

/// "Lists spacing" (Appearance).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinesHeight {
    /// 38 dp rows (default).
    #[default]
    Spaced,
    /// 34 dp rows.
    Compact,
}

/// Avatar source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AvatarProvider {
    /// Initials only.
    None,
    /// Gravatar by author e-mail hash.
    #[default]
    Gravatar,
}

/// Diff layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffViewMode {
    /// One column.
    #[default]
    Unified,
    /// Side by side.
    Split,
}

/// Proxy protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyType {
    /// HTTP proxy.
    #[default]
    Http,
    /// SOCKS proxy.
    Socks,
}

/// Shell of the embedded terminal (D0.4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalShell {
    /// First available of the platform defaults.
    #[default]
    Auto,
    /// PowerShell 7.
    Pwsh,
    /// Windows PowerShell.
    WindowsPowerShell,
    /// `cmd.exe`.
    Cmd,
    /// Git Bash.
    GitBash,
    /// Windows Subsystem for Linux.
    Wsl,
    /// A user-supplied command line.
    Custom(String),
}

/// Style of generated commit messages (D0.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiCommitStyle {
    /// Conventional Commits.
    #[default]
    Conventional,
    /// A plain imperative sentence.
    PlainImperative,
}

/// Update channel (D0.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateChannel {
    /// Stable releases.
    #[default]
    Release,
    /// Nightly builds.
    Nightly,
}

/// Application settings. Every field has a default so older and newer files load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeKind,
    /// Gitnuro-format custom theme JSON (used when `theme` is `Custom`).
    pub custom_theme: String,
    pub lines_height: LinesHeight,
    /// UI scale on top of the OS scale factor (1.0 = 100 %).
    pub scale: f32,
    pub avatar_provider: AvatarProvider,
    pub group_branches_with_remotes: bool,
    pub swap_status_panes: bool,
    pub show_diff_with_log: bool,
    pub date_use_system: bool,
    pub date_custom_format: String,
    pub date_24h: bool,
    pub date_relative: bool,
    pub diff_view: DiffViewMode,
    pub diff_full_file: bool,
    pub changes_as_tree: bool,
    pub clone_default_directory: Option<PathBuf>,
    pub git_path: Option<PathBuf>,
    /// Record undo history (`OplogOptions::enabled`).
    pub record_undo_history: bool,
    /// `RetentionPolicy::max_entries`.
    pub undo_keep_entries: usize,
    /// `RetentionPolicy::max_age` in days.
    pub undo_keep_days: u32,
    /// `OplogOptions::capture_untracked`.
    pub undo_capture_untracked: bool,
    pub fast_forward_merge: bool,
    pub auto_stash_on_merge: bool,
    pub pull_with_rebase: bool,
    pub push_with_lease: bool,
    pub use_proxy: bool,
    pub proxy_type: ProxyType,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_auth: bool,
    pub proxy_user: String,
    pub verify_ssl: bool,
    pub ssh_ask_new_hosts: bool,
    pub terminal_shell: TerminalShell,
    pub terminal_font_size: f32,
    /// Opaque AI provider settings; `null` means no provider.
    pub ai: serde_json::Value,
    pub ai_commit_style: AiCommitStyle,
    /// The `{provider}@{host}` the user confirmed sending code to (empty: none); a different provider or host asks
    /// again. Files of earlier versions hold a bool: `true` is read as the provider selected at load time.
    #[serde(deserialize_with = "consent_from_bool_or_text")]
    pub ai_hosted_consent: String,
    pub forge_https_answers: bool,
    /// Check for updates every 24 hours.
    pub update_check: bool,
    pub update_channel: UpdateChannel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Dark,
            custom_theme: String::new(),
            lines_height: LinesHeight::Spaced,
            scale: 1.0,
            avatar_provider: AvatarProvider::Gravatar,
            group_branches_with_remotes: true,
            swap_status_panes: false,
            show_diff_with_log: false,
            date_use_system: true,
            date_custom_format: "dd MMM yyyy".into(),
            date_24h: true,
            date_relative: true,
            diff_view: DiffViewMode::Unified,
            diff_full_file: false,
            changes_as_tree: false,
            clone_default_directory: None,
            git_path: None,
            record_undo_history: true,
            undo_keep_entries: 500,
            undo_keep_days: 30,
            undo_capture_untracked: true,
            fast_forward_merge: true,
            auto_stash_on_merge: true,
            pull_with_rebase: false,
            push_with_lease: true,
            use_proxy: false,
            proxy_type: ProxyType::Http,
            proxy_host: String::new(),
            proxy_port: 0,
            proxy_auth: false,
            proxy_user: String::new(),
            verify_ssl: true,
            ssh_ask_new_hosts: false,
            terminal_shell: TerminalShell::Auto,
            terminal_font_size: 13.0,
            ai: serde_json::Value::Null,
            ai_commit_style: AiCommitStyle::Conventional,
            ai_hosted_consent: String::new(),
            forge_https_answers: true,
            update_check: true,
            update_channel: UpdateChannel::Release,
        }
    }
}

/// What a `true` of the old boolean consent is read as until [`Settings::load_from`] knows the provider.
const LEGACY_CONSENT: &str = "*";

fn consent_from_bool_or_text<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrText {
        Bool(bool),
        Text(String),
    }
    Ok(match BoolOrText::deserialize(deserializer)? {
        BoolOrText::Bool(true) => LEGACY_CONSENT.to_owned(),
        BoolOrText::Bool(false) => String::new(),
        BoolOrText::Text(text) => text,
    })
}

/// The scale factors offered in Appearance: 75 % to 300 % in steps of 25 (09 §3.18).
pub const SCALE_CHOICES: [f32; 10] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 2.75, 3.0];

impl Settings {
    /// Loads settings from `path`: defaults when missing, defaults plus a backup when corrupt, the defaults of the
    /// keys whose values do not fit plus a backup otherwise (see [`load`]). The scale is clamped to the supported
    /// range.
    pub fn load_from(path: &Path) -> Loaded<Settings> {
        load::<Settings>(path).map(Settings::sanitized)
    }

    /// Writes the settings to `path` atomically.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, self)
    }

    fn sanitized(mut self) -> Self {
        if self.ai_hosted_consent == LEGACY_CONSENT {
            // The old bool confirmed the provider that was selected then, which is the one selected now.
            #[cfg(feature = "services")]
            {
                self.ai_hosted_consent =
                    crate::backend::ai::consent_key(&crate::backend::ai::parse_settings(&self.ai))
                        .unwrap_or_default();
            }
            #[cfg(not(feature = "services"))]
            self.ai_hosted_consent.clear();
        }
        let (lo, hi) = (SCALE_CHOICES[0], SCALE_CHOICES[SCALE_CHOICES.len() - 1]);
        self.scale = if self.scale.is_finite() {
            self.scale.clamp(lo, hi)
        } else {
            1.0
        };
        self
    }
}

/// The settings global.
pub struct SettingsGlobal(pub Settings);

impl Global for SettingsGlobal {}

/// Installs `settings`, as read from `settings.json`, as the global (without scheduling a save).
pub fn init(cx: &mut App, settings: Settings) {
    set_base(cx, "settings", &settings);
    cx.set_global(SettingsGlobal(settings));
}

/// The current settings. Panics if [`init`] has not run (a programming error).
pub fn get(cx: &App) -> &Settings {
    &cx.global::<SettingsGlobal>().0
}

/// The current settings, or defaults when [`init`] has not run (for code that may run before startup finished).
pub fn get_or_default(cx: &App) -> Settings {
    cx.try_global::<SettingsGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

/// Changes the settings: runs `edit`, notifies `observe_global::<SettingsGlobal>` observers when anything changed
/// and schedules a debounced save of `settings.json`.
pub fn update(cx: &mut App, edit: impl FnOnce(&mut Settings)) {
    let mut next = get_or_default(cx);
    let before = next.clone();
    edit(&mut next);
    if next == before && cx.has_global::<SettingsGlobal>() {
        return;
    }
    let next = next.sanitized();
    cx.set_global(SettingsGlobal(next.clone()));
    schedule_save(cx, "settings", crate::paths::settings_file(), next);
}

/// Writes settings and UI state immediately (on quit), each only if this process changed it, and then only the
/// changed keys (another instance may have changed others).
pub fn flush(cx: &App) {
    if let Some(s) = cx.try_global::<SettingsGlobal>() {
        io::flush_now(cx, "settings", &crate::paths::settings_file(), &s.0);
    }
    state::flush(cx);
}

#[cfg(test)]
mod tests;
