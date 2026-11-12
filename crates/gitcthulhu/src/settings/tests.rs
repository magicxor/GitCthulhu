use core::prelude::v1::test;

use std::path::PathBuf;

use super::*;

#[test]
fn settings_roundtrip_and_defaults() {
    let defaults = Settings::default();
    // 21 §7.2 defaults (Gitnuro's plus the D0 additions).
    assert_eq!(defaults.theme, ThemeKind::Dark, "theme default");
    assert_eq!(
        defaults.lines_height,
        LinesHeight::Spaced,
        "lists spacing default"
    );
    assert_eq!(defaults.scale, 1.0, "scale default");
    assert_eq!(
        defaults.avatar_provider,
        AvatarProvider::Gravatar,
        "avatar default"
    );
    assert!(
        defaults.group_branches_with_remotes,
        "group branches with remotes"
    );
    assert!(!defaults.swap_status_panes, "swap panes off");
    assert!(!defaults.show_diff_with_log, "diff replaces log");
    assert_eq!(
        defaults.date_custom_format, "dd MMM yyyy",
        "custom date format"
    );
    assert!(
        defaults.date_use_system && defaults.date_24h && defaults.date_relative,
        "date flags"
    );
    assert_eq!(defaults.diff_view, DiffViewMode::Unified, "diff view");
    assert!(
        defaults.record_undo_history && defaults.undo_capture_untracked,
        "undo recording is on"
    );
    assert_eq!(
        (defaults.undo_keep_entries, defaults.undo_keep_days),
        (500, 30),
        "undo retention"
    );
    assert!(
        defaults.fast_forward_merge && defaults.auto_stash_on_merge,
        "branch defaults"
    );
    assert!(
        !defaults.pull_with_rebase && defaults.push_with_lease,
        "remote action defaults"
    );
    assert!(
        defaults.verify_ssl && !defaults.ssh_ask_new_hosts,
        "security defaults"
    );
    assert_eq!(
        defaults.terminal_shell,
        TerminalShell::Auto,
        "terminal shell"
    );
    assert_eq!(defaults.terminal_font_size, 13.0, "terminal font size");
    assert!(defaults.ai.is_null(), "no AI provider by default");
    assert!(
        defaults.forge_https_answers && defaults.update_check,
        "service defaults"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.json");
    let changed = Settings {
        theme: ThemeKind::Light,
        scale: 1.25,
        lines_height: LinesHeight::Compact,
        clone_default_directory: Some(PathBuf::from("C:/src")),
        terminal_shell: TerminalShell::Custom("nu -l".into()),
        ai: serde_json::json!({"provider": "ollama"}),
        undo_keep_entries: 12,
        ..Settings::default()
    };
    changed.save_to(&path).expect("save");
    let Loaded::Ok(read) = Settings::load_from(&path) else {
        panic!("a saved file must load as Ok");
    };
    assert_eq!(read, changed, "settings survive a save/load roundtrip");

    std::fs::write(&path, br#"{"scale": 2.0, "unknown_future_key": 1}"#).expect("write partial");
    let read = Settings::load_from(&path).into_value();
    assert_eq!(read.scale, 2.0, "present keys are read");
    assert_eq!(
        read.theme,
        ThemeKind::Dark,
        "missing keys fall back to defaults"
    );
}

#[test]
fn settings_scale_is_clamped_on_load() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, br#"{"scale": 40.0}"#).expect("write");
    assert_eq!(
        Settings::load_from(&path).into_value().scale,
        3.0,
        "scale above the largest choice is clamped"
    );
}

#[test]
fn corrupt_settings_renamed_bak() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("settings.json");
    std::fs::write(&path, b"{ definitely not json").expect("write garbage");
    let Loaded::Corrupt { value, backup } = Settings::load_from(&path) else {
        panic!("garbage must be reported as corrupt");
    };
    assert_eq!(value, Settings::default(), "defaults replace the bad file");
    let backup = backup.expect("the bad file is renamed");
    assert_eq!(
        backup,
        dir.path().join("settings.json.bak"),
        "backup is `<name>.bak`"
    );
    assert!(backup.exists(), "backup exists");
    assert!(!path.exists(), "the corrupt original no longer exists");
}

#[test]
fn ui_state_roundtrip() {
    let defaults = UiState::default();
    assert_eq!(
        (
            defaults.first_pane_width,
            defaults.third_pane_width,
            defaults.terminal_height
        ),
        (220., 330., 240.),
        "pane and terminal defaults"
    );
    assert_eq!(
        defaults.window.placement,
        PlacementKind::Maximized,
        "first run is maximized"
    );
    assert_eq!(
        (defaults.window.w, defaults.window.h),
        (1280., 720.),
        "restore box"
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("state.json");
    let state = UiState {
        tabs_open: vec![PathBuf::from("C:/a"), PathBuf::from("C:/b")],
        selected_tab: 1,
        recent_repositories: vec![PathBuf::from("C:/b")],
        first_pane_width: 301.5,
        third_pane_width: 222.0,
        terminal_height: 180.0,
        window: WindowState {
            placement: PlacementKind::Floating,
            x: 10.,
            y: 20.,
            w: 900.,
            h: 700.,
        },
    };
    state.save_to(&path).expect("save");
    let Loaded::Ok(read) = UiState::load_from(&path) else {
        panic!("a saved file must load as Ok");
    };
    assert_eq!(read, state, "UI state survives a save/load roundtrip");
}

#[test]
fn ui_state_selected_tab_out_of_range_is_zero() {
    let state = UiState {
        tabs_open: vec![PathBuf::from("C:/a")],
        selected_tab: 5,
        ..UiState::default()
    };
    assert_eq!(
        state.selected_tab_index(),
        0,
        "invalid index falls back to 0"
    );
}

#[test]
fn recent_repositories_are_deduplicated_and_capped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir_all(&a).expect("mkdir a");
    std::fs::create_dir_all(&b).expect("mkdir b");
    let mut state = UiState::default();
    state.push_recent(&a);
    state.push_recent(&b);
    state.push_recent(&a);
    assert_eq!(
        state.recent_repositories.len(),
        2,
        "re-opening a repository does not duplicate it"
    );
    assert_eq!(
        state.recent_repositories[0],
        state::canonical(&a),
        "the most recent repository comes first"
    );
    // The same directory reached through `.` segments is the same repository.
    state.push_recent(&b.join("..").join("b"));
    assert_eq!(
        state.recent_repositories.len(),
        2,
        "paths are compared by canonical form"
    );

    for i in 0..(state::MAX_RECENT + 20) {
        state.push_recent(&PathBuf::from(format!("/nonexistent/repo-{i}")));
    }
    assert_eq!(
        state.recent_repositories.len(),
        state::MAX_RECENT,
        "the list is capped"
    );
}

#[test]
fn repo_prefs_roundtrip_and_drop_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    let mut prefs = RepoPrefs::default();
    prefs.edit(&repo, |p| {
        p.signoff = Signoff {
            enabled: true,
            text: "Signed-off-by: A <a@example.com>".into(),
        };
        p.forge_override = Some("github.com".into());
    });
    assert_eq!(prefs.len(), 1, "an edited repository is stored");
    let path = dir.path().join("repos.json");
    prefs.save_to(&path).expect("save");
    let Loaded::Ok(read) = RepoPrefs::load_from(&path) else {
        panic!("a saved file must load as Ok");
    };
    assert_eq!(read, prefs, "repo preferences survive a roundtrip");
    assert!(
        read.get(&repo).signoff.enabled,
        "preferences are found by canonical path"
    );
    prefs.edit(&repo, |p| *p = RepoPref::default());
    assert!(prefs.is_empty(), "default entries are dropped");
}

#[gpui_kit::test]
fn settings_update_notifies_and_debounces_the_save(cx: &mut gpui_kit::TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    // SAFETY-free: environment is only read by `paths`, and this is the only test that sets it.
    let target = dir.path().join("settings.json");
    cx.update(|cx| {
        init(cx, Settings::default());
        schedule_save(cx, "settings-test", target.clone(), Settings::default());
    });
    cx.executor().advance_clock(io::SAVE_DEBOUNCE / 2);
    cx.run_until_parked();
    assert!(!target.exists(), "nothing is written before the debounce");
    cx.executor().advance_clock(io::SAVE_DEBOUNCE);
    cx.run_until_parked();
    assert!(target.exists(), "the file is written after the debounce");
}
