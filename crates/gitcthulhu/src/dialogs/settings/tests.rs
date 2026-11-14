use core::prelude::v1::test;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, Entity, TestAppContext, Window};

use super::*;
use crate::app_view::AppView;
use crate::backend::{GitStatus, GitTime};
use crate::settings::{AvatarProvider, LinesHeight, ProxyType, Settings};
use crate::test_support::geometry::{
    assert_height, assert_painted_on_top, assert_rect, assert_width, bounds,
};
use crate::test_support::{
    DEFAULT_SIZE, RepoSeed, calls, init_app, mount_seeded, open_app, toast_texts, window_size,
};
use crate::theme::{ThemeKind, builtin, custom};
use crate::widgets::field::BorderState;

fn settings_of(cx: &mut TestAppContext) -> Settings {
    cx.update(|cx| settings::get_or_default(cx))
}

/// An app window of `size` with the Settings dialog open on `page`, drawn twice.
fn open_settings(
    cx: &mut TestAppContext,
    size: gpui_kit::Size<gpui_kit::Pixels>,
    page: Option<SettingsPage>,
) -> (AnyWindowHandle, Entity<AppView>) {
    init_app(cx);
    let (handle, app) = open_app(cx, size);
    show(cx, handle, page);
    (handle, app)
}

fn show(cx: &mut TestAppContext, handle: AnyWindowHandle, page: Option<SettingsPage>) {
    cx.update_window(handle, |_, window, cx| {
        open(window, cx, page);
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
}

fn view(cx: &mut TestAppContext) -> Entity<SettingsView> {
    cx.update(|cx| {
        cx.global::<OpenSettingsView>()
            .0
            .upgrade()
            .expect("the settings view is alive")
    })
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .expect("window");
    render(cx, handle);
}

/// Opens the dropdown `key` and picks option `index`.
fn pick(cx: &mut TestAppContext, handle: AnyWindowHandle, key: &str, index: usize) {
    click(cx, handle, ids::select(key));
    click(cx, handle, ids::select_option(key, index));
}

fn select_page(cx: &mut TestAppContext, handle: AnyWindowHandle, page: SettingsPage) {
    click(cx, handle, ids::entry(page));
}

fn advance(cx: &mut TestAppContext, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    cx.run_until_parked();
}

/// Types `text` over the content of the focused input (the kit input does not announce programmatic changes).
fn retype(window: &mut Window, cx: &mut App, text: &str) {
    window.render_frame(cx);
    window.press("secondary-a", cx);
    if text.is_empty() {
        window.press("backspace", cx);
    } else {
        window.input(text, cx);
    }
}

fn set_text(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    field: &Entity<OutlinedField>,
    text: &str,
) {
    cx.update_window(handle, |_, window, cx| {
        field.update(cx, |f, cx| f.focus(window, cx));
        retype(window, cx, text);
    })
    .expect("window");
    cx.run_until_parked();
}

fn set_number(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    field: &Entity<NumberField>,
    text: &str,
) {
    cx.update_window(handle, |_, window, cx| {
        field.update(cx, |f, cx| f.focus(window, cx));
        retype(window, cx, text);
    })
    .expect("window");
    cx.run_until_parked();
}

// ----- layout (21 §8.2) -----

#[gpui_kit::test]
fn geometry_1000x720(cx: &mut TestAppContext) {
    // The kit keeps 16 px clear around a dialog, so the full 720 dp need a window of 752 px or more.
    let (handle, _) = open_settings(cx, window_size(1280., 800.), None);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::DIALOG, 140., 40., 1000., 720.);
        assert_rect(window, ids::LEFT, 140., 40., 200., 720.);
        // Content column: 200 dp to the right of the frame start, 16 dp padding, 64 dp top padding.
        assert_rect(
            window,
            ids::row("theme"),
            140. + 200. + 16.,
            40. + 64.,
            768.,
            55.7,
        );
        // Accept: 16 dp from the right and bottom edges.
        let accept = bounds(window, ids::ACCEPT);
        assert!(
            ((accept.origin.x + accept.size.width).as_f32() - (140. + 1000. - 16.)).abs() <= 0.5,
            "Accept ends 16 dp from the right edge: {accept:?}"
        );
        assert!(
            ((accept.origin.y + accept.size.height).as_f32() - (40. + 720. - 16.)).abs() <= 0.5,
            "Accept ends 16 dp from the bottom edge: {accept:?}"
        );
        assert_width(window, ids::select("theme"), 180.);
    })
    .expect("window");
}

#[gpui_kit::test]
fn entries_are_32_high_and_sections_are_spaced(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    cx.update_window(handle, |_, window, _| {
        for page in [
            SettingsPage::Appearance,
            SettingsPage::DateTime,
            SettingsPage::Environment,
            SettingsPage::Logs,
        ] {
            assert_height(window, ids::entry(page), 32.);
            assert_width(window, ids::entry(page), 200.);
        }
        let appearance = bounds(window, ids::entry(SettingsPage::Appearance));
        let layout = bounds(window, ids::entry(SettingsPage::Layout));
        assert!(
            (layout.origin.y - appearance.origin.y).as_f32() - 32. <= 0.5,
            "entries of a section follow each other directly"
        );
        let date = bounds(window, ids::entry(SettingsPage::DateTime));
        let environment = bounds(window, ids::entry(SettingsPage::Environment));
        // 16 dp spacer plus a header (4 + 13 sp x 1.21 + 4).
        let gap = (environment.origin.y - (date.origin.y + date.size.height)).as_f32();
        assert!(
            (gap - (16. + 8. + 13. * 1.21)).abs() <= 0.6,
            "a section starts after a 16 dp spacer and its header, got {gap}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn controls_have_the_spec_widths(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, window_size(1280., 800.), Some(SettingsPage::Layout));
    cx.update_window(handle, |_, window, _| {
        let switch = bounds(window, ids::switch("swap-status-panes"));
        assert_eq!(
            (switch.size.width.as_f32(), switch.size.height.as_f32()),
            (48., 28.),
            "switch 48 x 28"
        );
        assert!(
            ((switch.origin.x + switch.size.width).as_f32() - (140. + 1000. - 16.)).abs() <= 0.5,
            "the control ends at the content padding"
        );
    })
    .expect("window");
    select_page(cx, handle, SettingsPage::Environment);
    cx.update_window(handle, |_, window, _| {
        assert_width(window, ids::field("clone-dir"), 240.);
        assert_width(window, ids::field("git-path"), 240.);
        assert_width(window, ids::field("undo-entries"), 136.);
        assert_width(window, ids::field("undo-days"), 136.);
    })
    .expect("window");
    select_page(cx, handle, SettingsPage::Appearance);
    pick(cx, handle, "theme", 2);
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, ids::row("custom-theme"));
        assert_rect(
            window,
            ids::THEME_EDITOR,
            140. + 1000. - 16. - 240.,
            row.origin.y.as_f32() + 8.,
            240.,
            120.,
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn the_dialog_never_exceeds_the_window(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::DIALOG, 140., 16., 1000., 688.);
        let accept = bounds(window, ids::ACCEPT);
        assert!(
            ((accept.origin.y + accept.size.height).as_f32() - (16. + 688. - 16.)).abs() <= 0.5,
            "Accept stays inside the window: {accept:?}"
        );
    })
    .expect("window");
    let (handle, _) = open_settings(cx, window_size(1000., 600.), None);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::DIALOG, 16., 16., 968., 568.);
    })
    .expect("window");
}

// ----- categories -----

#[gpui_kit::test]
fn m1_categories_equal_gitnuro(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    let sections = cx.update(|cx| categories(cx));
    let titles: Vec<(&str, Vec<&str>)> = sections
        .iter()
        .map(|(s, pages)| (s.title(), pages.iter().map(|p| p.title()).collect()))
        .collect();
    assert_eq!(
        titles,
        vec![
            ("User interface", vec!["Appearance", "Layout", "Date/Time"]),
            ("Git", vec!["Environment", "Branches", "Remote actions"]),
            ("Network", vec!["Proxy", "Authentication", "Security"]),
            ("Tools", vec!["Terminal", "Logs"]),
        ],
        "milestone 1 shows Gitnuro's categories only"
    );
    cx.update_window(handle, |_, window, _| {
        for page in [
            SettingsPage::Accounts,
            SettingsPage::Ai,
            SettingsPage::Updates,
        ] {
            assert!(
                window.try_find(ids::entry(page)).is_none(),
                "{page:?} has no entry in milestone 1"
            );
        }
    })
    .expect("window");
}

#[gpui_kit::test]
fn categories_and_selection(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    let v = view(cx);
    assert_eq!(
        cx.update(|cx| v.read(cx).page()),
        SettingsPage::Appearance,
        "Appearance is selected first"
    );
    for page in [
        SettingsPage::Layout,
        SettingsPage::DateTime,
        SettingsPage::Environment,
        SettingsPage::Branches,
        SettingsPage::RemoteActions,
        SettingsPage::Proxy,
        SettingsPage::Authentication,
        SettingsPage::Security,
        SettingsPage::Terminal,
        SettingsPage::Logs,
    ] {
        select_page(cx, handle, page);
        assert_eq!(cx.update(|cx| v.read(cx).page()), page, "{page:?} selected");
        cx.update_window(handle, |_, window, _| {
            assert!(
                window.try_find(ids::row("theme")).is_none() || page == SettingsPage::Appearance,
                "the rows of {page:?} replace those of Appearance"
            );
        })
        .expect("window");
    }
}

#[gpui_kit::test]
fn opening_twice_switches_the_page_instead_of_stacking(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    show(cx, handle, Some(SettingsPage::Security));
    let v = view(cx);
    assert_eq!(cx.update(|cx| v.read(cx).page()), SettingsPage::Security);
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx), "one Escape closes it");
    })
    .expect("window");
    show(cx, handle, Some(SettingsPage::Proxy));
    let v = view(cx);
    assert_eq!(
        cx.update(|cx| v.read(cx).page()),
        SettingsPage::Proxy,
        "a closed dialog opens fresh at the requested page"
    );
    show(cx, handle, Some(SettingsPage::Accounts));
    assert_eq!(
        cx.update(|cx| v.read(cx).page()),
        SettingsPage::Proxy,
        "an unavailable category is ignored"
    );
}

#[gpui_kit::test]
fn accept_closes_and_escape_closes(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    click(cx, handle, ids::ACCEPT);
    cx.update_window(handle, |_, window, cx| {
        assert!(!window.has_active_dialog(cx), "Accept closes the dialog");
    })
    .expect("window");
    show(cx, handle, None);
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx), "Escape closes the dialog");
    })
    .expect("window");
}

// ----- behaviour -----

#[gpui_kit::test]
fn changes_apply_immediately(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Layout));
    assert!(!settings_of(cx).swap_status_panes);
    click(cx, handle, ids::switch("swap-status-panes"));
    assert!(
        settings_of(cx).swap_status_panes,
        "the switch writes at once"
    );
    click(cx, handle, ids::switch("swap-status-panes"));
    assert!(!settings_of(cx).swap_status_panes, "and back");
    click(cx, handle, ids::switch("show-diff-with-log"));
    assert!(settings_of(cx).show_diff_with_log);
    select_page(cx, handle, SettingsPage::Branches);
    click(cx, handle, ids::switch("fast-forward"));
    click(cx, handle, ids::switch("auto-stash"));
    let s = settings_of(cx);
    assert!(!s.fast_forward_merge && !s.auto_stash_on_merge);
    select_page(cx, handle, SettingsPage::RemoteActions);
    click(cx, handle, ids::switch("pull-rebase"));
    click(cx, handle, ids::switch("push-lease"));
    let s = settings_of(cx);
    assert!(s.pull_with_rebase && !s.push_with_lease);
    select_page(cx, handle, SettingsPage::Appearance);
    click(cx, handle, ids::switch("group-branches"));
    assert!(!settings_of(cx).group_branches_with_remotes);
    pick(cx, handle, "lines-height", 1);
    assert_eq!(settings_of(cx).lines_height, LinesHeight::Compact);
    pick(cx, handle, "avatar-provider", 0);
    assert_eq!(settings_of(cx).avatar_provider, AvatarProvider::None);
}

#[gpui_kit::test]
fn theme_switch_live(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    pick(cx, handle, "theme", 0);
    assert_eq!(settings_of(cx).theme, ThemeKind::Light);
    cx.update(|cx| {
        assert_eq!(
            cx.theme_g().kind,
            ThemeKind::Light,
            "the active theme follows the setting"
        );
        assert_eq!(cx.tokens().background, builtin::light().background);
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.has_active_dialog(cx), "the dialog stays open");
    })
    .expect("window");
    pick(cx, handle, "theme", 1);
    cx.update(|cx| assert_eq!(cx.tokens().background, builtin::dark().background));
}

#[gpui_kit::test]
fn custom_theme_starts_from_the_current_theme_and_hides_with_other_themes(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::THEME_EDITOR).is_none(),
            "the editor shows only for the Custom theme"
        );
    })
    .expect("window");
    pick(cx, handle, "theme", 2);
    let s = settings_of(cx);
    assert_eq!(s.theme, ThemeKind::Custom);
    assert!(
        custom::parse(&s.custom_theme).is_ok() && s.custom_theme.contains("\"primaryVariant\""),
        "an empty custom theme starts from a full export: {}",
        s.custom_theme
    );
    cx.update(|cx| {
        assert_eq!(cx.theme_g().kind, ThemeKind::Custom, "it applies");
        assert_eq!(cx.tokens().background, builtin::dark().background);
    });
    cx.update_window(handle, |_, window, _| {
        window.find(ids::THEME_EDITOR);
    })
    .expect("window");
    pick(cx, handle, "theme", 1);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::THEME_EDITOR).is_none());
    })
    .expect("window");
}

#[gpui_kit::test]
fn custom_theme_edits_apply_after_one_second_and_errors_fall_back_to_dark(cx: &mut TestAppContext) {
    let (handle, app) = open_settings(cx, DEFAULT_SIZE, None);
    pick(cx, handle, "theme", 2);
    let v = view(cx);
    let editor = cx.update(|cx| v.read(cx).fields.custom_theme.clone());
    let edit = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(handle, |_, window, cx| {
            editor.update(cx, |e, cx| e.focus(window, cx));
            retype(window, cx, text);
        })
        .expect("window");
        cx.run_until_parked();
    };
    edit(cx, r#"{"background": "FF102030", "isLight": false}"#);
    assert_ne!(
        settings_of(cx).custom_theme,
        r#"{"background": "FF102030", "isLight": false}"#,
        "nothing is applied before the quiet period ends"
    );
    advance(cx, 1000);
    cx.update(|cx| {
        assert_eq!(cx.theme_g().kind, ThemeKind::Custom);
        assert_eq!(
            crate::theme::custom::to_argb(cx.tokens().background),
            0xFF102030,
            "the edited colour is live"
        );
    });
    assert!(
        cx.update(|cx| toast_texts(&app, cx)).is_empty(),
        "a valid theme shows no toast"
    );

    edit(cx, "{ not json");
    advance(cx, 1000);
    cx.update(|cx| {
        assert_eq!(
            cx.theme_g().kind,
            ThemeKind::Dark,
            "an unusable theme falls back to Dark"
        );
    });
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.contains(&"Failed trying to save the custom theme".to_owned()),
        "the Error toast of 21 §3.1 is shown: {toasts:?}"
    );
    assert!(
        cx.update(|cx| editor.read(cx).has_error()),
        "the editor shows the error border"
    );
    assert_eq!(
        settings_of(cx).theme,
        ThemeKind::Custom,
        "the choice stays Custom"
    );

    edit(cx, r#"{"background": "FF203040"}"#);
    advance(cx, 1000);
    cx.update(|cx| assert_eq!(cx.theme_g().kind, ThemeKind::Custom));
    assert!(cx.update(|cx| !editor.read(cx).has_error()));
}

#[gpui_kit::test]
fn closing_inside_the_quiet_period_keeps_the_edits(cx: &mut TestAppContext) {
    init_app(cx);
    let (store, cell) = SecretStore::in_memory();
    cx.update(|cx| {
        cx.set_global(SecretStoreGlobal(store));
        settings::update(cx, |s| {
            s.use_proxy = true;
            s.proxy_auth = true;
        })
    });
    let (handle, _) = open_app(cx, DEFAULT_SIZE);
    show(cx, handle, Some(SettingsPage::Appearance));
    pick(cx, handle, "theme", 2);
    let v = view(cx);
    let editor = cx.update(|cx| v.read(cx).fields.custom_theme.clone());
    cx.update_window(handle, |_, window, cx| {
        editor.update(cx, |e, cx| e.focus(window, cx));
        retype(window, cx, r#"{"background": "FF010203"}"#);
    })
    .expect("window");
    cx.run_until_parked();
    select_page(cx, handle, SettingsPage::Proxy);
    let password = cx.update(|cx| v.read(cx).fields.proxy_password.clone());
    set_text(cx, handle, &password, "pw");
    click(cx, handle, ids::ACCEPT);
    cx.run_until_parked();
    assert_eq!(
        settings_of(cx).custom_theme,
        r#"{"background": "FF010203"}"#,
        "Accept inside the 1 s quiet period still stores the theme"
    );
    assert_eq!(
        cell.lock().expect("lock").as_deref(),
        Some("pw"),
        "and the password"
    );
}

#[gpui_kit::test]
fn escape_inside_the_quiet_period_keeps_the_edit_too(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    pick(cx, handle, "theme", 2);
    let v = view(cx);
    let editor = cx.update(|cx| v.read(cx).fields.custom_theme.clone());
    cx.update_window(handle, |_, window, cx| {
        editor.update(cx, |e, cx| e.focus(window, cx));
        retype(window, cx, r#"{"background": "FF040506"}"#);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx), "Escape closed the dialog");
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        settings_of(cx).custom_theme,
        r#"{"background": "FF040506"}"#,
        "Escape inside the quiet period still stores the theme"
    );
}

#[gpui_kit::test]
fn scale_changes_rem(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    cx.update_window(handle, |_, window, _| {
        assert_eq!(
            window.rem_size(),
            gpui_kit::px(14.),
            "100 % is the 14 px base"
        );
    })
    .expect("window");
    // Options: 75, 100, 125, 150, 175, 200, 250, 300.
    pick(cx, handle, "scale", 5);
    assert_eq!(settings_of(cx).scale, 2.0);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.rem_size(),
            gpui_kit::px(28.),
            "200 % doubles the rem size"
        );
        let frame = bounds(window, ids::DIALOG);
        assert!(
            frame.size.width.as_f32() <= 1280.5 && frame.size.height.as_f32() <= 720.5,
            "the dialog is clamped to the window: {frame:?}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn a_non_standard_scale_is_listed_too(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(|cx| settings::update(cx, |s| s.scale = 1.1));
    let (handle, _) = open_app(cx, DEFAULT_SIZE);
    show(cx, handle, None);
    click(cx, handle, ids::select("scale"));
    cx.update_window(handle, |_, window, _| {
        window.find(ids::select_option("scale", 2));
        assert!(
            window.try_find(ids::select_option("scale", 10)).is_some(),
            "eleven options: the ten standard scales and 110%"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn proxy_keys_correct(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Proxy));
    let v = view(cx);
    let (host, user) = cx.update(|cx| {
        (
            v.read(cx).fields.proxy_host.clone(),
            v.read(cx).fields.proxy_user.clone(),
        )
    });
    click(cx, handle, ids::switch("use-proxy"));
    click(cx, handle, ids::switch("proxy-auth"));
    set_text(cx, handle, &host, "proxy.local");
    set_text(cx, handle, &user, "alice");
    let s = settings_of(cx);
    assert!(s.use_proxy && s.proxy_auth);
    assert_eq!(s.proxy_host, "proxy.local", "the host goes to the host key");
    assert_eq!(s.proxy_user, "alice", "the login goes to the login key");
    assert!(s.verify_ssl, "the proxy fields leave the SSL setting alone");
    pick(cx, handle, "proxy-type", 1);
    assert_eq!(settings_of(cx).proxy_type, ProxyType::Socks);

    select_page(cx, handle, SettingsPage::Security);
    click(cx, handle, ids::switch("no-ssl-verify"));
    let s = settings_of(cx);
    assert!(!s.verify_ssl, "the Security switch writes verify_ssl");
    assert_eq!(s.proxy_user, "alice", "and nothing else");
    assert!(s.pull_with_rebase == Settings::default().pull_with_rebase);
    click(cx, handle, ids::switch("no-ssl-verify"));
    assert!(
        settings_of(cx).verify_ssl,
        "switching it off again verifies"
    );
    click(cx, handle, ids::switch("ssh-ask-hosts"));
    assert!(settings_of(cx).ssh_ask_new_hosts);
}

#[gpui_kit::test]
fn proxy_rows_follow_the_enable_switches(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Proxy));
    let v = view(cx);
    let state = |cx: &mut TestAppContext, handle: AnyWindowHandle| {
        cx.update_window(handle, |_, window, cx| {
            let f = &v.read(cx).fields;
            let border = |field: &Entity<OutlinedField>| field.read(cx).border_state(window, cx);
            (
                border(&f.proxy_host) == BorderState::Disabled,
                border(&f.proxy_user) == BorderState::Disabled,
                border(&f.proxy_password) == BorderState::Disabled,
            )
        })
        .expect("window")
    };
    assert_eq!(state(cx, handle), (true, true, true), "all off by default");
    click(cx, handle, ids::switch("use-proxy"));
    assert_eq!(
        state(cx, handle),
        (false, true, true),
        "the host needs the proxy, login and password also need authentication"
    );
    click(cx, handle, ids::switch("proxy-auth"));
    assert_eq!(state(cx, handle), (false, false, false));
}

#[gpui_kit::test]
fn the_proxy_password_goes_to_the_secret_store_only(cx: &mut TestAppContext) {
    init_app(cx);
    let (store, cell) = SecretStore::in_memory();
    cx.update(|cx| cx.set_global(SecretStoreGlobal(store)));
    let (handle, _) = open_app(cx, DEFAULT_SIZE);
    cx.update(|cx| {
        settings::update(cx, |s| {
            s.use_proxy = true;
            s.proxy_auth = true;
        })
    });
    show(cx, handle, Some(SettingsPage::Proxy));
    cx.run_until_parked();
    let v = view(cx);
    assert_eq!(
        cx.update(|cx| v.read(cx).password_state().clone()),
        PasswordState::Stored(false),
        "the keychain answered: no password yet"
    );
    let field = cx.update(|cx| v.read(cx).fields.proxy_password.clone());
    set_text(cx, handle, &field, "s3cret");
    assert!(
        cell.lock().expect("lock").is_none(),
        "nothing is written while typing"
    );
    advance(cx, 500);
    assert_eq!(
        cell.lock().expect("lock").as_deref(),
        Some("s3cret"),
        "the password reaches the secret store after the quiet period"
    );
    assert_eq!(
        cx.update(|cx| v.read(cx).password_state().clone()),
        PasswordState::Stored(true)
    );
    let json = serde_json::to_string(&settings_of(cx)).expect("settings serialise");
    assert!(!json.contains("s3cret"), "the settings file never holds it");
    set_text(cx, handle, &field, "");
    advance(cx, 500);
    assert_eq!(
        cell.lock().expect("lock").as_deref(),
        Some("s3cret"),
        "an emptied field never deletes the stored secret"
    );
    assert_eq!(
        cx.update(|cx| v.read(cx).password_state().clone()),
        PasswordState::Stored(true)
    );
    click(cx, handle, "settings-proxy-password-remove");
    advance(cx, 500);
    assert!(
        cell.lock().expect("lock").is_none(),
        "the Remove link deletes it"
    );
    assert_eq!(
        cx.update(|cx| v.read(cx).password_state().clone()),
        PasswordState::Stored(false)
    );
}

#[gpui_kit::test]
fn closing_with_an_empty_password_field_keeps_the_stored_secret(cx: &mut TestAppContext) {
    init_app(cx);
    let (store, cell) = SecretStore::in_memory();
    (store.store)("kept").expect("store");
    cx.update(|cx| {
        cx.set_global(SecretStoreGlobal(store));
        settings::update(cx, |s| {
            s.use_proxy = true;
            s.proxy_auth = true;
        })
    });
    let (handle, _) = open_app(cx, DEFAULT_SIZE);
    show(cx, handle, Some(SettingsPage::Proxy));
    cx.run_until_parked();
    let v = view(cx);
    let field = cx.update(|cx| v.read(cx).fields.proxy_password.clone());
    set_text(cx, handle, &field, "x");
    set_text(cx, handle, &field, "");
    click(cx, handle, ids::ACCEPT);
    cx.run_until_parked();
    assert_eq!(
        cell.lock().expect("lock").as_deref(),
        Some("kept"),
        "Accept inside the quiet period with an empty field leaves the secret alone"
    );
}

#[gpui_kit::test]
fn date_format_preview(cx: &mut TestAppContext) {
    // 2026-11-12 17:30 UTC.
    let now = GitTime {
        seconds: 1_794_504_600,
        offset_minutes: 0,
    };
    assert_eq!(date_preview(now, None).as_deref(), Some("Nov 12, 2026"));
    assert_eq!(
        date_preview(now, Some("dd MMM yyyy")).as_deref(),
        Some("12 Nov 2026")
    );
    assert_eq!(
        date_preview(now, Some("EEEE, d MMMM yyyy")).as_deref(),
        Some("Thursday, 12 November 2026")
    );
    assert_eq!(
        date_preview(now, Some("xx")),
        None,
        "invalid patterns have no preview"
    );
    assert_eq!(pages::time_preview(now, true), "17:30");
    assert_eq!(pages::time_preview(now, false), "05:30 PM");

    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::DateTime));
    let v = view(cx);
    let field = cx.update(|cx| v.read(cx).fields.date_format.clone());
    let border = |cx: &mut TestAppContext| {
        cx.update_window(handle, |_, window, cx| {
            field.read(cx).border_state(window, cx)
        })
        .expect("window")
    };
    assert_eq!(
        border(cx),
        BorderState::Disabled,
        "the custom format is disabled while the system format is used"
    );
    click(cx, handle, ids::switch("date-use-system"));
    assert!(!settings_of(cx).date_use_system);
    assert_ne!(border(cx), BorderState::Disabled, "enabled now");
    set_text(cx, handle, &field, "yyyy-MM-dd");
    assert_eq!(settings_of(cx).date_custom_format, "yyyy-MM-dd");
    assert_ne!(border(cx), BorderState::Error);
    set_text(cx, handle, &field, "qq");
    assert_eq!(border(cx), BorderState::Error, "invalid: error border");
    assert_eq!(
        settings_of(cx).date_custom_format,
        "yyyy-MM-dd",
        "an invalid pattern is not stored"
    );
    click(cx, handle, ids::switch("date-24h"));
    click(cx, handle, ids::switch("date-relative"));
    let s = settings_of(cx);
    assert!(!s.date_24h && !s.date_relative);
}

#[test]
fn date_patterns_are_validated_like_java_for_the_supported_letters() {
    for ok in [
        "dd MMM yyyy",
        "yyyy-MM-dd HH:mm",
        "d.M.yy",
        "EEEE, d MMMM yyyy",
        "hh:mm a",
        "'at' H",
        "yyyy 'o''clock'",
    ] {
        assert!(valid_date_pattern(ok), "{ok:?} is valid");
    }
    for bad in [
        "",
        "abc",
        "'open",
        "ddd",
        "aa",
        "---",
        "MMMMM",
        "G",
        "yyyy-MM-dd z",
        "HHH",
    ] {
        assert!(!valid_date_pattern(bad), "{bad:?} is not valid");
    }
}

#[gpui_kit::test]
fn environment_fields_write_clone_directory_and_git_path(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Environment));
    let v = view(cx);
    let (clone, git) = cx.update(|cx| {
        (
            v.read(cx).fields.clone_dir.clone(),
            v.read(cx).fields.git_path.clone(),
        )
    });
    set_text(cx, handle, &clone, "C:\\work");
    set_text(cx, handle, &git, "  ");
    let s = settings_of(cx);
    assert_eq!(s.clone_default_directory, Some(PathBuf::from("C:\\work")));
    assert_eq!(s.git_path, None, "blank means automatic");
    set_text(cx, handle, &clone, "");
    assert_eq!(settings_of(cx).clone_default_directory, None);
}

#[gpui_kit::test]
fn undo_rows_apply_to_open_tabs(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    show(cx, handle, Some(SettingsPage::Environment));
    let applied = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            calls(&tab, cx)
                .into_iter()
                .filter(|c| {
                    matches!(
                        c,
                        crate::test_support::TestCall::Op {
                            kind: crate::tab::ops::OpKind::ApplyUndoSettings,
                            ..
                        }
                    )
                })
                .count()
        })
    };
    assert_eq!(applied(cx), 0);
    click(cx, handle, ids::switch("undo-record"));
    cx.run_until_parked();
    assert!(!settings_of(cx).record_undo_history);
    assert_eq!(applied(cx), 1, "turning recording off reaches the open tab");
    assert!(!cx.update(|cx| tab.read(cx).applied_undo_settings().enabled));

    let v = view(cx);
    let entries = cx.update(|cx| v.read(cx).fields.undo_entries.clone());
    set_number(cx, handle, &entries, "42");
    assert_eq!(settings_of(cx).undo_keep_entries, 42);
    assert_eq!(
        cx.update(|cx| tab.read(cx).applied_undo_settings().keep_entries),
        42,
        "the retention limit reaches the open tab"
    );
    let days = cx.update(|cx| v.read(cx).fields.undo_days.clone());
    set_number(cx, handle, &days, "7");
    assert_eq!(settings_of(cx).undo_keep_days, 7);
    click(cx, handle, ids::switch("undo-capture-untracked"));
    cx.run_until_parked();
    assert!(!settings_of(cx).undo_capture_untracked);
    assert!(!cx.update(|cx| tab.read(cx).applied_undo_settings().capture_untracked));
}

#[gpui_kit::test]
fn number_fields_take_digits_only_and_flash_on_invalid_input(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Environment));
    let v = view(cx);
    let days = cx.update(|cx| v.read(cx).fields.undo_days.clone());
    cx.update_window(handle, |_, window, cx| {
        days.update(cx, |f, cx| f.focus(window, cx));
        window.render_frame(cx);
        // Replace the default "30" by typing over it.
        window.press("secondary-a", cx);
        window.input("1a2b", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| days.read(cx).text(cx).to_string()),
        "12",
        "letters are removed"
    );
    assert!(
        cx.update(|cx| days.read(cx).is_flashing()),
        "the border flashes"
    );
    assert_eq!(settings_of(cx).undo_keep_days, 12);
    advance(cx, 500);
    assert!(
        !cx.update(|cx| days.read(cx).is_flashing()),
        "the flash ends after 500 ms"
    );
    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("0", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        settings_of(cx).undo_keep_days,
        12,
        "zero days is not stored"
    );
    assert!(cx.update(|cx| days.read(cx).is_flashing()), "and flashes");
}

/// A paste with a non-digit is cleaned on screen and the clean value is stored (`set_value` emits no change event).
#[gpui_kit::test]
fn a_pasted_number_with_junk_is_cleaned_and_stored(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Proxy));
    let v = view(cx);
    let port = cx.update(|cx| v.read(cx).fields.proxy_port.clone());
    click(cx, handle, ids::switch("use-proxy"));
    cx.update_window(handle, |_, window, cx| {
        port.update(cx, |f, cx| f.focus(window, cx));
        window.render_frame(cx);
        window.press("secondary-a", cx);
    })
    .expect("window");
    cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            " 3128
"
            .into(),
        ))
    });
    cx.update_window(handle, |_, window, cx| window.press("secondary-v", cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| port.read(cx).text(cx).to_string()), "3128");
    assert_eq!(
        settings_of(cx).proxy_port,
        3128,
        "the clean value is stored"
    );
    assert!(
        cx.update(|cx| port.read(cx).is_flashing()),
        "and it flashes"
    );
}

#[gpui_kit::test]
fn the_proxy_port_is_limited_to_65535(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Proxy));
    let v = view(cx);
    let port = cx.update(|cx| v.read(cx).fields.proxy_port.clone());
    click(cx, handle, ids::switch("use-proxy"));
    set_number(cx, handle, &port, "3128");
    assert_eq!(settings_of(cx).proxy_port, 3128);
    set_number(cx, handle, &port, "70000");
    assert_eq!(settings_of(cx).proxy_port, 3128, "too large: not stored");
    assert!(cx.update(|cx| port.read(cx).is_flashing()));
}

#[gpui_kit::test]
fn authentication_and_terminal_pages_are_gitnuros_with_the_m1_changes(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Authentication));
    cx.update_window(handle, |_, window, _| {
        window.find(ids::row("credentials"));
        assert!(
            window
                .try_find(ids::switch("forge-https-answers"))
                .is_none(),
            "no account toggle without connected accounts"
        );
    })
    .expect("window");
    select_page(cx, handle, SettingsPage::Terminal);
    // With the embedded terminal (X1) its own rows replace Gitnuro's row (21 §5.4; tested in `terminal::tests`).
    #[cfg(feature = "terminal")]
    cx.update_window(handle, |_, window, _| {
        window.find(ids::row("terminal-shell"));
        assert!(window.try_find(ids::row("terminal-path")).is_none());
    })
    .expect("window");
    #[cfg(not(feature = "terminal"))]
    cx.update_window(handle, |_, window, cx| {
        window.find(ids::row("terminal-path"));
        let v = view_sync(cx);
        let border = v
            .read(cx)
            .fields
            .terminal_path
            .read(cx)
            .border_state(window, cx);
        assert_eq!(
            border,
            BorderState::Disabled,
            "Gitnuro's row is disabled in M1"
        );
    })
    .expect("window");
}

#[cfg(not(feature = "terminal"))]
fn view_sync(cx: &mut App) -> Entity<SettingsView> {
    cx.global::<OpenSettingsView>()
        .0
        .upgrade()
        .expect("the settings view is alive")
}

#[gpui_kit::test]
fn logs_page_opens_the_log_folder(cx: &mut TestAppContext) {
    init_app(cx);
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let record = opened.clone();
    cx.update(|cx| {
        cx.set_global(OpenFolderHook(Rc::new(move |path, _| {
            record.borrow_mut().push(path.to_path_buf())
        })))
    });
    let (handle, _) = open_app(cx, DEFAULT_SIZE);
    show(cx, handle, Some(SettingsPage::Logs));
    click(cx, handle, "settings-open-logs");
    assert_eq!(
        opened.borrow().as_slice(),
        [crate::paths::log_dir()],
        "the button opens the logs folder"
    );
}

// ----- the Git runtime follows the Git settings -----

fn git_status(cx: &mut TestAppContext) -> GitStatus {
    cx.update(|cx| crate::app::git_status(cx))
}

#[gpui_kit::test]
fn git_executable_change_rebuilds_the_runtime_for_new_repositories(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::empty(), DEFAULT_SIZE);
    show(cx, handle, Some(SettingsPage::Environment));
    let v = view(cx);
    let git = cx.update(|cx| v.read(cx).fields.git_path.clone());
    assert!(matches!(git_status(cx), GitStatus::Starting));

    set_text(cx, handle, &git, "C:\\no\\such\\git.exe");
    advance(cx, 300);
    assert!(
        matches!(git_status(cx), GitStatus::Starting),
        "a change waits for a quiet period (typing must not start git per keystroke)"
    );
    advance(cx, 400);
    assert!(
        matches!(git_status(cx), GitStatus::Failed(_)),
        "an unusable executable is reported, got {:?}",
        git_status(cx)
    );
    let toasts = cx.update(|cx| {
        let view = cx
            .global::<crate::app_view::AppViewGlobal>()
            .0
            .upgrade()
            .expect("app view");
        toast_texts(&view, cx)
    });
    assert!(
        toasts.contains(&"Reopen the repository to apply the new Git settings".to_owned()),
        "open tabs are told to reopen: {toasts:?}"
    );
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        window.find(ids::row("git-path"));
    })
    .expect("window");

    set_text(cx, handle, &git, "");
    advance(cx, 700);
    assert!(
        matches!(git_status(cx), GitStatus::Ready(_)),
        "automatic detection finds git again, got {:?}",
        git_status(cx)
    );
}

#[gpui_kit::test]
fn changes_that_do_not_touch_git_do_not_rebuild_and_reverts_cancel(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, Some(SettingsPage::Layout));
    click(cx, handle, ids::switch("swap-status-panes"));
    advance(cx, 2000);
    assert!(
        matches!(git_status(cx), GitStatus::Starting),
        "a layout change leaves the runtime alone"
    );
    select_page(cx, handle, SettingsPage::Security);
    click(cx, handle, ids::switch("no-ssl-verify"));
    click(cx, handle, ids::switch("no-ssl-verify"));
    advance(cx, 2000);
    assert!(
        matches!(git_status(cx), GitStatus::Starting),
        "a change reverted inside the quiet period needs no rebuild"
    );
    click(cx, handle, ids::switch("no-ssl-verify"));
    advance(cx, 2000);
    assert!(
        matches!(git_status(cx), GitStatus::Ready(_)),
        "turning SSL verification off rebuilds the runtime"
    );
}

#[test]
fn runtime_keys_cover_exactly_the_git_settings() {
    let base = Settings::default();
    let key = |f: &dyn Fn(&mut Settings)| {
        let mut s = base.clone();
        f(&mut s);
        RuntimeKey::of(&s, 0)
    };
    let reference = RuntimeKey::of(&base, 0);
    assert_eq!(key(&|s| s.swap_status_panes = true), reference);
    assert_eq!(
        key(&|s| s.undo_keep_days = 3),
        reference,
        "undo keys apply per tab"
    );
    assert_ne!(key(&|s| s.git_path = Some("git".into())), reference);
    assert_ne!(key(&|s| s.use_proxy = true), reference);
    assert_ne!(key(&|s| s.proxy_host = "h".into()), reference);
    assert_ne!(key(&|s| s.proxy_port = 1), reference);
    assert_ne!(key(&|s| s.proxy_auth = true), reference);
    assert_ne!(key(&|s| s.proxy_user = "u".into()), reference);
    assert_ne!(key(&|s| s.proxy_type = ProxyType::Socks), reference);
    assert_ne!(key(&|s| s.verify_ssl = false), reference);
    assert_ne!(key(&|s| s.ssh_ask_new_hosts = true), reference);
    assert_ne!(
        RuntimeKey::of(&base, 1),
        reference,
        "a stored password rebuilds too"
    );
}

// ----- theme/custom.rs -----

#[test]
fn custom_theme_parse_errors() {
    assert!(custom::parse("not json").is_err(), "garbage");
    assert!(custom::parse("[1, 2]").is_err(), "an array is not a theme");
    assert!(custom::parse("").is_err(), "empty text");
    let err = custom::parse(r#"{"primary": 5}"#).expect_err("a number is not a colour");
    assert!(err.0.contains("primary"), "the key is named: {err}");
    let err = custom::parse(r#"{"surface": "zzz"}"#).expect_err("not hex");
    assert!(err.0.contains("surface"), "{err}");
    let err = custom::parse(r#"{"background": ""}"#).expect_err("empty colour");
    assert!(err.0.contains("background"), "{err}");
    assert!(
        custom::parse(r#"{"isLight": "yes"}"#).is_err(),
        "isLight is a boolean"
    );
    assert!(
        custom::parse(r#"{"surface": "123456789012345678"}"#).is_err(),
        "too many digits"
    );
    // Valid inputs.
    let t = custom::parse(r##"{"primary": "#FF112233", "unknownKey": 7, "isLight": true}"##)
        .expect("a partial theme with an unknown key is fine");
    assert_eq!(custom::to_argb(t.primary), 0xFF112233);
    assert!(t.is_light);
    assert_eq!(
        t.background,
        builtin::dark().background,
        "missing keys take Dark"
    );
    assert!(
        custom::parse("{}").is_ok(),
        "an empty object is the Dark theme"
    );
}

#[test]
fn custom_theme_export_writes_every_key_and_round_trips() {
    for tokens in [builtin::dark(), builtin::light()] {
        let json = custom::export(&tokens);
        let value: serde_json::Value = serde_json::from_str(&json).expect("export is JSON");
        let object = value.as_object().expect("an object");
        assert_eq!(object.len(), 29, "28 colours plus isLight");
        for key in custom::colour_keys() {
            let text = object
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("{key} is exported"));
            assert_eq!(text.len(), 8, "{key} is 8 hex digits: {text}");
        }
        assert_eq!(object["isLight"], serde_json::Value::Bool(tokens.is_light));
        assert_eq!(
            custom::parse(&json).expect("the export parses"),
            tokens,
            "export then parse gives the same tokens"
        );
    }
    assert_eq!(custom::colour_keys().count(), 28);
    assert!(custom::export(&builtin::dark()).contains("\"primary\": \"FF0169CB\""));
}

#[test]
fn argb_hex_parsing_follows_compose() {
    assert_eq!(custom::parse_argb("FF0070D8"), Some(0xFF0070D8));
    assert_eq!(custom::parse_argb("#ff0070d8"), Some(0xFF0070D8));
    assert_eq!(
        custom::parse_argb("0070D8"),
        Some(0x000070D8),
        "six digits: alpha 0, like toLong(16)"
    );
    assert_eq!(
        custom::parse_argb("FFFFFFFFF"),
        Some(0xFFFFFFFF),
        "nine digits are masked to 32 bits (07 §13.1)"
    );
    assert_eq!(custom::parse_argb(""), None);
    assert_eq!(custom::parse_argb("xyz"), None);
}

// ----- review additions -----

#[gpui_kit::test]
fn escape_closes_an_open_dropdown_before_the_dialog(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, DEFAULT_SIZE, None);
    click(cx, handle, ids::select("theme"));
    let v = view(cx);
    assert!(cx.update(|cx| v.read(cx).is_select_open("theme")));
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(
            window.has_active_dialog(cx),
            "the first Escape closes only the popup (09 §4.0)"
        );
        assert!(window.try_find(ids::select_popup("theme")).is_none());
    })
    .expect("window");
    assert!(cx.update(|cx| !v.read(cx).is_select_open("theme")));
    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(
            !window.has_active_dialog(cx),
            "the next Escape closes the dialog"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn dropdown_popup_uses_the_menu_chrome(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, window_size(1280., 800.), None);
    click(cx, handle, ids::select("theme"));
    cx.update_window(handle, |_, window, _| {
        let trigger = bounds(window, ids::select("theme"));
        let popup = bounds(window, ids::select_popup("theme"));
        assert!(
            (popup.origin.x - trigger.origin.x).as_f32().abs() <= 0.5
                && (popup.origin.y - (trigger.origin.y + trigger.size.height))
                    .as_f32()
                    .abs()
                    <= 0.5,
            "the popup hangs from the trigger's bottom-left: {trigger:?} {popup:?}"
        );
        assert!(
            popup.size.width.as_f32() >= 180. - 0.5,
            "min width 180: {popup:?}"
        );
        // Item: padding v8 around one body2 line (13 sp x 1.21), about 32 dp (09 §4.0).
        let item = bounds(window, ids::select_option("theme", 0));
        assert!(
            (item.size.height.as_f32() - (16. + 13. * 1.21)).abs() <= 0.6,
            "item height {item:?}"
        );
        // Trigger: 180 wide, 8 + 24 + 8 high (padding v8 around the 24 dp chevron).
        assert_eq!(trigger.size.width.as_f32(), 180.);
        assert!(
            (trigger.size.height.as_f32() - 40.).abs() <= 0.5,
            "{trigger:?}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn dropdown_popups_are_painted_over_the_dialog(cx: &mut TestAppContext) {
    // The dialog is a deferred layer of its own: a popup of a lower priority took the clicks but stayed hidden
    // under it.
    let (handle, _) = open_settings(cx, window_size(1280., 800.), None);
    for key in ["theme", "scale"] {
        click(cx, handle, ids::select(key));
        cx.update_window(handle, |_, window, cx| {
            assert_painted_on_top(window, ids::select_popup(key));
            window.press("escape", cx);
        })
        .expect("window");
        render(cx, handle);
    }
}

#[cfg(feature = "terminal")]
#[gpui_kit::test]
fn shell_popup_is_painted_over_the_dialog(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, window_size(1280., 800.), Some(SettingsPage::Terminal));
    click(cx, handle, ids::select(terminal::SHELL));
    cx.update_window(handle, |_, window, _| {
        assert_painted_on_top(window, ids::select_popup(terminal::SHELL));
    })
    .expect("window");
}

#[gpui_kit::test]
fn empty_subtitles_keep_their_line(cx: &mut TestAppContext) {
    let (handle, _) = open_settings(cx, window_size(1280., 800.), Some(SettingsPage::Proxy));
    cx.update_window(handle, |_, window, _| {
        // Title body1 (14 sp x 1.21) + 4 + subtitle body2 (13 sp x 1.21), as in Gitnuro, where Host name, Port
        // number and Login pass "" as the subtitle.
        let expected = 14. * 1.21 + 4. + 13. * 1.21;
        for key in ["proxy-host", "proxy-port", "proxy-user", "use-proxy"] {
            let titles = bounds(window, ids::titles(key));
            assert!(
                (titles.size.height.as_f32() - expected).abs() <= 0.6,
                "{key}: titles {titles:?}"
            );
        }
        // Row: padding v8 around the 38 dp field.
        assert_height(window, ids::row("proxy-host"), 54.);
    })
    .expect("window");
}

#[gpui_kit::test]
fn choosing_custom_with_an_unusable_stored_theme_says_so(cx: &mut TestAppContext) {
    init_app(cx);
    cx.update(|cx| settings::update(cx, |s| s.custom_theme = "{ broken".into()));
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    show(cx, handle, None);
    pick(cx, handle, "theme", 2);
    assert_eq!(settings_of(cx).theme, ThemeKind::Custom);
    assert_eq!(
        settings_of(cx).custom_theme,
        "{ broken",
        "the stored text is kept"
    );
    cx.update(|cx| assert_eq!(cx.theme_g().kind, ThemeKind::Dark, "Dark is shown"));
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.contains(&"Failed trying to save the custom theme".to_owned()),
        "{toasts:?}"
    );
    let v = view(cx);
    let editor = cx.update(|cx| v.read(cx).fields.custom_theme.clone());
    assert!(cx.update(|cx| editor.read(cx).has_error()));
}
