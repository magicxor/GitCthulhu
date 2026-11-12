//! The Gitnuro settings pages (owned by G11; 09 §3.18, spec 21 §5.4): Appearance, Layout, Date/Time, Environment
//! (with the undo rows), Branches, Remote actions, Proxy, Authentication, Security, Terminal, Logs.
//!
//! Every page is a list of [`SettingRow`]s built from the current settings; a control writes its change with
//! `settings::update` at once. Rows whose value cannot be stored in a plain control (text, numbers, the theme
//! editor) use the entities of [`SettingsView`].

use std::rc::Rc;

use chrono::{DateTime, FixedOffset};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};

use super::{Pick, SettingRow, SettingsView, ids, open_folder, sm};
use crate::backend::{GitStatus, GitTime};
use crate::metrics::dp;
use crate::settings::{self, AvatarProvider, LinesHeight, ProxyType, SCALE_CHOICES, Settings};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, ThemeKind};
use crate::widgets::date::{DateFormat, format_date, format_pattern, local_offset_minutes};
use crate::widgets::{AppSwitch, PrimaryButton};

/// The pattern letters [`format_pattern`] implements, with the longest run each accepts.
const PATTERN_LETTERS: [(char, usize); 9] = [
    ('y', usize::MAX),
    ('M', 4),
    ('d', 2),
    ('E', 4),
    ('H', 2),
    ('h', 2),
    ('m', 2),
    ('s', 2),
    ('a', 1),
];

/// Whether `pattern` is a date pattern the app can show: Java `DateTimeFormatter` syntax limited to the letters
/// `y M d E H h m s a` (runs of at most the length Java accepts), quoted text with `''` for a quote, any other
/// character verbatim, and at least one letter.
pub fn valid_date_pattern(pattern: &str) -> bool {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    let mut has_field = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\'') {
                i += 2;
                continue;
            }
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    closed = true;
                    i += 1;
                    break;
                }
                i += 1;
            }
            if !closed {
                return false;
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            let Some((_, longest)) = PATTERN_LETTERS.iter().find(|(l, _)| *l == c) else {
                return false;
            };
            let mut run = 1;
            while chars.get(i + run) == Some(&c) {
                run += 1;
            }
            if run > *longest {
                return false;
            }
            has_field = true;
            i += run;
            continue;
        }
        i += 1;
    }
    has_field
}

/// How `now` is shown: with the system format for `pattern == None`, else with the custom `pattern`. `None` when
/// the pattern is not valid. Relative words ("Today") are never used here: the preview shows the format.
pub fn date_preview(now: GitTime, pattern: Option<&str>) -> Option<String> {
    let format = match pattern {
        None => DateFormat {
            use_system: true,
            custom_pattern: String::new(),
            h24: true,
            relative: false,
        },
        Some(p) if valid_date_pattern(p) => DateFormat {
            use_system: false,
            custom_pattern: p.to_owned(),
            h24: true,
            relative: false,
        },
        Some(_) => return None,
    };
    Some(format_date(now, now, now.offset_minutes, &format, false))
}

/// The time of `now` as `17:30` (24 h) or `05:30 PM`.
pub fn time_preview(now: GitTime, h24: bool) -> String {
    let offset = FixedOffset::east_opt(now.offset_minutes.saturating_mul(60));
    let at =
        offset.and_then(|o| DateTime::from_timestamp(now.seconds, 0).map(|t| t.with_timezone(&o)));
    at.map(|at| format_pattern(if h24 { "HH:mm" } else { "hh:mm a" }, &at))
        .unwrap_or_default()
}

/// The current time in the machine's zone.
fn now() -> GitTime {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    GitTime {
        seconds,
        offset_minutes: local_offset_minutes(seconds),
    }
}

/// "75%" for 0.75.
fn percent(scale: f32) -> String {
    format!("{}%", (scale * 100.).round() as i32)
}

/// A row with a switch bound to a boolean setting.
fn switch_row(
    key: &'static str,
    title: &'static str,
    subtitle: impl Into<SharedString>,
    checked: bool,
    write: fn(&mut Settings, bool),
) -> AnyElement {
    switch_row_enabled(key, title, subtitle, checked, true, write)
}

/// [`switch_row`] that can be disabled (switch not clickable, titles dimmed).
fn switch_row_enabled(
    key: &'static str,
    title: &'static str,
    subtitle: impl Into<SharedString>,
    checked: bool,
    enabled: bool,
    write: fn(&mut Settings, bool),
) -> AnyElement {
    SettingRow::new(key, title)
        .subtitle(subtitle)
        .disabled(!enabled)
        .control(
            AppSwitch::new(ids::switch(key), checked)
                .enabled(enabled)
                .on_toggle(move |value, _, cx| settings::update(cx, |s| write(s, value))),
        )
        .into_any_element()
}

/// A pick that only writes a setting.
fn write_pick(write: impl Fn(&mut Settings) + 'static) -> Pick {
    Rc::new(move |_, _, cx| settings::update(cx, |s| write(s)))
}

impl SettingsView {
    pub(super) fn appearance_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        let theme_label = match s.theme {
            ThemeKind::Dark => "Dark",
            ThemeKind::Light => "Light",
            ThemeKind::Custom => "Custom",
        };
        let themes: Vec<(SharedString, Pick)> = [
            ("Light", ThemeKind::Light),
            ("Dark", ThemeKind::Dark),
            ("Custom", ThemeKind::Custom),
        ]
        .into_iter()
        .map(|(label, kind)| {
            let pick: Pick = Rc::new(move |this, window, cx| this.choose_theme(kind, window, cx));
            (SharedString::from(label), pick)
        })
        .collect();
        let theme_select = self.select("theme", theme_label, themes, cx);

        let spacing_label = match s.lines_height {
            LinesHeight::Spaced => "Spaced",
            LinesHeight::Compact => "Compact",
        };
        let spacing = vec![
            (
                SharedString::from("Spaced"),
                write_pick(|s| s.lines_height = LinesHeight::Spaced),
            ),
            (
                SharedString::from("Compact"),
                write_pick(|s| s.lines_height = LinesHeight::Compact),
            ),
        ];
        let spacing_select = self.select("lines-height", spacing_label, spacing, cx);

        // The standard scales, plus the current value when it is none of them (a hand-edited settings file).
        let mut scales: Vec<f32> = SCALE_CHOICES.to_vec();
        if !scales.iter().any(|v| (v - s.scale).abs() < 1e-3) {
            scales.push(s.scale);
            scales.sort_by(f32::total_cmp);
        }
        let scale_options: Vec<(SharedString, Pick)> = scales
            .into_iter()
            .map(|v| {
                (
                    SharedString::from(percent(v)),
                    write_pick(move |s| s.scale = v),
                )
            })
            .collect();
        let scale_select = self.select("scale", percent(s.scale), scale_options, cx);

        let avatar_label = match s.avatar_provider {
            AvatarProvider::None => "None",
            AvatarProvider::Gravatar => "Gravatar",
        };
        let avatars = vec![
            (
                SharedString::from("None"),
                write_pick(|s| s.avatar_provider = AvatarProvider::None),
            ),
            (
                SharedString::from("Gravatar"),
                write_pick(|s| s.avatar_provider = AvatarProvider::Gravatar),
            ),
        ];
        let avatar_select = self.select("avatar-provider", avatar_label, avatars, cx);

        let mut rows = vec![
            SettingRow::new("theme", "Theme")
                .subtitle("Select the UI theme between light and dark mode")
                .control(theme_select)
                .into_any_element(),
        ];
        if s.theme == ThemeKind::Custom {
            rows.push(
                SettingRow::new("custom-theme", "Custom theme")
                    .subtitle("Add a JSON to load the custom theme")
                    .top_aligned()
                    .control(self.fields.custom_theme.clone())
                    .into_any_element(),
            );
        }
        rows.push(
            SettingRow::new("lines-height", "Lists spacing (Beta)")
                .subtitle("Spacing around lists items")
                .control(spacing_select)
                .into_any_element(),
        );
        rows.push(
            SettingRow::new("scale", "Scale")
                .subtitle("Adapt the size the UI to your preferred scale")
                .control(scale_select)
                .into_any_element(),
        );
        rows.push(
            SettingRow::new("avatar-provider", "Avatar provider")
                .subtitle("When using a provider, the e-mail addresses will be hashed using SHA256")
                .control(avatar_select)
                .into_any_element(),
        );
        rows.push(switch_row(
            "group-branches",
            "Group local branches with their remotes",
            "Grouping of local branches with their remote when they are in the same commit",
            s.group_branches_with_remotes,
            |s, v| s.group_branches_with_remotes = v,
        ));
        rows
    }

    pub(super) fn layout_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        vec![
            switch_row(
                "swap-status-panes",
                "Swap position for staged/unstaged views",
                "Show the list of unstaged changes above the list of staged changes",
                s.swap_status_panes,
                |s, v| s.swap_status_panes = v,
            ),
            switch_row(
                "show-diff-with-log",
                "Show diff view below the commits history",
                "Allows you to keep both visible at the same time",
                s.show_diff_with_log,
                |s, v| s.show_diff_with_log = v,
            ),
        ]
    }

    pub(super) fn date_time_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        let now = now();
        let system = date_preview(now, None).unwrap_or_default();
        let pattern = self.fields.date_format.read(cx).text(cx);
        let custom = match date_preview(now, Some(&pattern)) {
            Some(text) => format!("Current date would be shown as \"{text}\""),
            None => "Invalid date/time format".to_owned(),
        };
        vec![
            switch_row(
                "date-use-system",
                "Use system's Date/Time format",
                format!("If enabled, current date would be shown as \"{system}\""),
                s.date_use_system,
                |s, v| s.date_use_system = v,
            ),
            SettingRow::new("date-format", "Custom date format")
                .subtitle(custom)
                .disabled(s.date_use_system)
                .control(
                    div()
                        .w(dp(sm::TEXT_FIELD_W))
                        .child(self.fields.date_format.clone()),
                )
                .into_any_element(),
            switch_row_enabled(
                "date-24h",
                "Use 24h time",
                format!(
                    "\"{}\" if on, \"{}\" if off",
                    time_preview(now, true),
                    time_preview(now, false)
                ),
                s.date_24h,
                !s.date_use_system,
                |s, v| s.date_24h = v,
            ),
            switch_row(
                "date-relative",
                "Relative date",
                "Use \"Today\" and \"Yesterday\" instead of the date",
                s.date_relative,
                |s, v| s.date_relative = v,
            ),
        ]
    }

    pub(super) fn environment_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        let tokens = *cx.tokens();
        let caption = |text: &'static str| {
            div()
                .text_body2()
                .text_color(tokens.on_background_secondary)
                .child(text)
        };
        let mut git = SettingRow::new("git-path", "Git executable")
            .subtitle("Path of the git program. Leave it empty to find git automatically.")
            .control(
                div()
                    .w(dp(sm::TEXT_FIELD_W))
                    .child(self.fields.git_path.clone()),
            );
        if let GitStatus::Failed(err) = crate::app::git_status(cx) {
            git = git.note(err.to_string(), true);
        }
        vec![
            SettingRow::new("clone-dir", "Default clone directory")
                .subtitle("Directory that will be used as default when cloning a repository")
                .control(
                    div()
                        .w(dp(sm::TEXT_FIELD_W))
                        .child(self.fields.clone_dir.clone()),
                )
                .into_any_element(),
            git.into_any_element(),
            switch_row(
                "undo-record",
                "Record undo history",
                "Keep a snapshot before every change so that Undo and Redo can restore it",
                s.record_undo_history,
                |s, v| s.record_undo_history = v,
            ),
            SettingRow::new("undo-keep", "Keep undo history for")
                .subtitle("Older entries are removed when either limit is exceeded")
                .control(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(dp(8.))
                        .child(self.fields.undo_entries.clone())
                        .child(caption("operations"))
                        .child(self.fields.undo_days.clone())
                        .child(caption("days")),
                )
                .into_any_element(),
            switch_row(
                "undo-capture-untracked",
                "Let Undo restore deleted untracked files",
                "Restorable copies are stored in the repository under refs/gitcthulhu and are copied by git push \
                 --mirror / git clone --mirror. Ignored files at restored paths are protected only while this is on.",
                s.undo_capture_untracked,
                |s, v| s.undo_capture_untracked = v,
            ),
        ]
    }

    pub(super) fn branches_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        vec![
            switch_row(
                "fast-forward",
                "Fast-forward merge",
                "Try to fast-forward merges when possible",
                s.fast_forward_merge,
                |s, v| s.fast_forward_merge = v,
            ),
            switch_row(
                "auto-stash",
                "Automatically stash uncommitted changes before merge",
                "To avoid losing work if the merge is aborted, the app can create a snapshot of the uncommitted \
                 changes",
                s.auto_stash_on_merge,
                |s, v| s.auto_stash_on_merge = v,
            ),
        ]
    }

    pub(super) fn remote_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        vec![
            switch_row(
                "pull-rebase",
                "Pull with rebase as default",
                "Rebase changes instead of merging when pulling",
                s.pull_with_rebase,
                |s, v| s.pull_with_rebase = v,
            ),
            switch_row(
                "push-lease",
                "Force push with lease",
                "Check if the local version remote branch is up to date to avoid accidentally overriding \
                 unintended commits",
                s.push_with_lease,
                |s, v| s.push_with_lease = v,
            ),
        ]
    }

    pub(super) fn proxy_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        let type_label = match s.proxy_type {
            ProxyType::Http => "HTTP",
            ProxyType::Socks => "SOCKS",
        };
        let types = vec![
            (
                SharedString::from("HTTP"),
                write_pick(|s| s.proxy_type = ProxyType::Http),
            ),
            (
                SharedString::from("SOCKS"),
                write_pick(|s| s.proxy_type = ProxyType::Socks),
            ),
        ];
        let type_select = self.select("proxy-type", type_label, types, cx);
        let auth = s.use_proxy && s.proxy_auth;
        let password_note = match &self.password {
            super::PasswordState::Unavailable(reason) => {
                Some(format!("The system keychain is not available: {reason}"))
            }
            super::PasswordState::Stored(true) => Some(
                "A password is saved in the system keychain. Type a new one to replace it."
                    .to_owned(),
            ),
            _ => None,
        };
        let mut password = SettingRow::new("proxy-password", "Password")
            .subtitle("Saved in the system keychain, never in the settings file")
            .disabled(!auth)
            .control(
                div()
                    .w(dp(sm::TEXT_FIELD_W))
                    .child(self.fields.proxy_password.clone()),
            );
        if let Some(note) = password_note {
            password = password.note(note, false);
        }
        vec![
            switch_row(
                "use-proxy",
                "Use proxy",
                "Set up your proxy configuration if needed",
                s.use_proxy,
                |s, v| s.use_proxy = v,
            ),
            SettingRow::new("proxy-type", "Proxy type")
                .subtitle("Pick between HTTP or SOCKS")
                .control(type_select)
                .into_any_element(),
            SettingRow::new("proxy-host", "Host name")
                // Gitnuro passes an empty subtitle, which still takes its line.
                .subtitle("")
                .disabled(!s.use_proxy)
                .control(
                    div()
                        .w(dp(sm::TEXT_FIELD_W))
                        .child(self.fields.proxy_host.clone()),
                )
                .into_any_element(),
            SettingRow::new("proxy-port", "Port number")
                // Gitnuro passes an empty subtitle, which still takes its line.
                .subtitle("")
                .disabled(!s.use_proxy)
                .control(self.fields.proxy_port.clone())
                .into_any_element(),
            switch_row(
                "proxy-auth",
                "Proxy authentication",
                "Use your credentials to provide your identity the proxy server",
                s.proxy_auth,
                |s, v| s.proxy_auth = v,
            ),
            SettingRow::new("proxy-user", "Login")
                // Gitnuro passes an empty subtitle, which still takes its line.
                .subtitle("")
                .disabled(!auth)
                .control(
                    div()
                        .w(dp(sm::TEXT_FIELD_W))
                        .child(self.fields.proxy_user.clone()),
                )
                .into_any_element(),
            password.into_any_element(),
        ]
    }

    pub(super) fn authentication_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        let mut rows = vec![
            SettingRow::new("credentials", "Credentials")
                .subtitle(
                    "GitCthulhu never stores HTTPS credentials. Git's credential helper (for example Git \
                     Credential Manager) handles them.",
                )
                .into_any_element(),
        ];
        if crate::forge::accounts::available(cx) {
            rows.push(switch_row(
                "forge-https-answers",
                "Offer connected accounts for HTTPS prompts",
                "Use a signed-in GitHub, GitLab or Bitbucket account when Git asks for HTTPS credentials",
                s.forge_https_answers,
                |s, v| s.forge_https_answers = v,
            ));
        }
        rows
    }

    pub(super) fn security_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = settings::get_or_default(cx);
        vec![
            // The switch shows the opposite of `verify_ssl`; Gitnuro wrote it into the credential-cache key
            // (09 §5.8).
            switch_row(
                "no-ssl-verify",
                "Do not verify SSL security",
                "If active, you may connect to the remote server via insecure HTTPS connection",
                !s.verify_ssl,
                |s, v| s.verify_ssl = !v,
            ),
            switch_row(
                "ssh-ask-hosts",
                "Ask before trusting new SSH hosts",
                "Ask before connecting to an SSH server whose host key is not known yet. When off, new host keys \
                 are accepted and remembered.",
                s.ssh_ask_new_hosts,
                |s, v| s.ssh_ask_new_hosts = v,
            ),
        ]
    }

    pub(super) fn terminal_rows(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        if crate::terminal::settings_available(cx) {
            return vec![super::terminal::page(window, cx)];
        }
        // Gitnuro's row, disabled until the embedded terminal exists (21 §5.4).
        vec![
            SettingRow::new("terminal-path", "Custom terminal path")
                .subtitle("The embedded terminal arrives in a later version")
                .disabled(true)
                .control(
                    div()
                        .w(dp(sm::TEXT_FIELD_W))
                        .child(self.fields.terminal_path.clone()),
                )
                .into_any_element(),
        ]
    }

    pub(super) fn logs_rows(&mut self, _cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            SettingRow::new("logs", "Logs")
                .subtitle("Open the logs folder")
                .control(
                    PrimaryButton::new("settings-open-logs", "Open folder").on_click(|_, cx| {
                        open_folder(cx, &crate::paths::log_dir());
                    }),
                )
                .into_any_element(),
        ]
    }
}
