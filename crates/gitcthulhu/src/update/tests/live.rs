use core::prelude::v1::test;

use std::sync::{Arc, Mutex};

use gpui_kit::{AnyWindowHandle, IntoElement as _, ParentElement as _, Styled as _};

use super::*;
use crate::backend::services::ServicesHandle;
use crate::backend::update::UpdateCheckError;
use crate::dialogs::settings::{self as settings_dialog, SettingsPage};
use crate::settings::AvatarProvider;
use crate::test_support::geometry::{bounds, open_probe};
use crate::test_support::{DEFAULT_SIZE, open_app, toast_texts};
use crate::update::loopback::{Loopback, Response, manifest};
use crate::widgets::avatar::{Avatar, gravatar_url};

struct Env {
    server: Loopback,
    manifest: Arc<Mutex<(u16, String)>>,
    avatar: Arc<Mutex<(u16, Vec<u8>)>>,
    dir: tempfile::TempDir,
}

impl Env {
    fn new(version: &str) -> Self {
        let manifest_body = Arc::new(Mutex::new((
            200,
            manifest(version, "https://example.com/releases/v1"),
        )));
        let avatar = Arc::new(Mutex::new((200, PNG.to_vec())));
        let (m, a) = (manifest_body.clone(), avatar.clone());
        let server = Loopback::start(move |path| {
            let reply = |status: u16, body: Vec<u8>, content_type: &'static str| Response {
                status,
                content_type,
                body,
            };
            if path.starts_with("/manifest") {
                let (status, body) = m.lock().expect("manifest lock").clone();
                reply(status, body.into_bytes(), "application/json")
            } else if path.starts_with("/avatar/") {
                let (status, body) = a.lock().expect("avatar lock").clone();
                reply(status, body, "image/png")
            } else {
                Response::status(404)
            }
        })
        .expect("loopback server");
        Self {
            server,
            manifest: manifest_body,
            avatar,
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn serve_manifest(&self, version: &str) {
        *self.manifest.lock().expect("lock") =
            (200, manifest(version, "https://example.com/releases/v1"));
    }

    fn services(&self, app_version: &str) -> ServicesHandle {
        crate::backend::update::services_with_manifest(
            self.dir.path().join("data"),
            self.dir.path().join("cache"),
            Some(&self.server.url("/manifest.json")),
            app_version,
        )
        .expect("services start")
    }

    fn manifest_requests(&self) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|p| p.starts_with("/manifest"))
            .count()
    }

    fn avatar_requests(&self) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|p| p.starts_with("/avatar/"))
            .count()
    }
}

/// Records the URLs the app asks the OS to open.
fn frame<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        f(window, cx)
    })
    .expect("window")
}

fn set_update_check(cx: &mut TestAppContext, on: bool) {
    cx.update(|cx| settings::update(cx, |s| s.update_check = on));
}

fn snapshot_of(cx: &mut TestAppContext) -> UpdateSnapshot {
    cx.update(|cx| snapshot(cx))
}

fn install_services(cx: &mut TestAppContext, services: ServicesHandle) {
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("app state").services = Some(services);
    });
    cx.run_until_parked();
}

fn has_toast(
    cx: &mut TestAppContext,
    app: &gpui_kit::Entity<crate::app_view::AppView>,
    text: &str,
) -> bool {
    cx.run_until_parked();
    cx.update(|cx| toast_texts(app, cx))
        .iter()
        .any(|t| t.contains(text))
}

// ---------------------------------------------------------------------------------------------------------------
// the notice in the bottom bar
// ---------------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn notice_shown_when_available(cx: &mut TestAppContext) {
    init_app(cx);
    let urls = record_urls(cx);
    let (handle, _app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("9.9.9");

    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::NOTICE).is_none(),
            "without services there is no notice"
        );
    });

    install_services(cx, env.services("0.1.0"));
    assert_eq!(env.manifest_requests(), 1, "the startup check asked once");
    assert_eq!(
        snapshot_of(cx).notice,
        Some(UpdateNotice {
            version: "9.9.9".into(),
            url: "https://example.com/releases/v1".into()
        })
    );

    frame(cx, handle, |window, cx| {
        let button = bounds(window, ids::NOTICE);
        let version = bounds(window, crate::shell::ids::BOTTOM_VERSION);
        let bar = bounds(window, crate::ids::BOTTOM_BAR);
        assert!(
            (version.left().as_f32() - button.right().as_f32() - 16.).abs() <= 0.5,
            "the notice ends 16 dp before the version link: {button:?} {version:?}"
        );
        assert!(
            ((button.top() + button.bottom()).as_f32() - (bar.top() + bar.bottom()).as_f32()).abs()
                <= 1.,
            "the notice is centred vertically in the 32 dp bar"
        );
        assert_eq!(bar.size.height.as_f32(), 32., "the 32 dp bottom bar");
        assert!(
            (button.size.height.as_f32() - 23.7).abs() <= 1.5,
            "Gitnuro's SecondaryButton: padding V4 around body2, about 23.7 dp: {button:?}"
        );
        assert!(
            button.left() > bar.left() + gpui_kit::px(100.),
            "the notice sits at the right end, not at the identity side"
        );
        window.click(ids::NOTICE, cx);
    });
    assert_eq!(
        *urls.borrow(),
        ["https://example.com/releases/v1".to_owned()],
        "a click opens the download link"
    );
    assert_eq!(notice::label("9.9.9"), "Update 9.9.9 available");
}

#[gpui_kit::test]
fn remind_later_snoozes(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("9.9.9");
    install_services(cx, env.services("0.1.0"));
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::NOTICE).is_some());
    });

    // Right-click -> "Remind me tomorrow".
    cx.update_window(handle, |_, window, cx| {
        window.right_click(ids::NOTICE, cx);
        window.render_frame(cx);
        window.click("menu-item-update-remind-tomorrow", cx);
    })
    .expect("window");
    cx.run_until_parked();

    assert!(
        has_toast(cx, &app, "remind you about this update tomorrow"),
        "the snooze is confirmed"
    );
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::NOTICE).is_none(), "the notice is gone");
    });
    assert_eq!(snapshot_of(cx).notice, None);

    // The service remembers the snooze: the next check does not bring the notice back.
    cx.update(|cx| check_now(cx, CheckKind::Manual));
    cx.run_until_parked();
    assert_eq!(
        snapshot_of(cx).notice,
        None,
        "a snoozed update stays hidden after another check"
    );
    assert_eq!(env.manifest_requests(), 2, "the manual check did ask");

    // A snooze of a week is accepted by the service as well.
    cx.update(|cx| snooze(cx, SNOOZE_WEEK_HOURS));
    cx.run_until_parked();
    assert!(has_toast(cx, &app, "in a week"));
    assert_eq!(
        snooze_text(5),
        "We will remind you about this update in 5 hours."
    );
}

/// The remind-later menu opens above the notice, its bottom-left corner at the notice's top-left corner, so it never
/// hides the notice or the "Version" link beside it.
#[gpui_kit::test]
fn remind_later_menu_opens_above_the_notice(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, _app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("9.9.9");
    install_services(cx, env.services("0.1.0"));
    frame(cx, handle, |_, _| ());
    cx.update_window(handle, |_, window, cx| window.right_click(ids::NOTICE, cx))
        .expect("window");
    frame(cx, handle, |window, _| {
        let notice = bounds(window, ids::NOTICE);
        let version = bounds(window, crate::shell::ids::BOTTOM_VERSION);
        let menu = bounds(window, crate::ids::MENU);
        assert!(
            (menu.bottom().as_f32() - notice.top().as_f32()).abs() <= 1.
                && (menu.left().as_f32() - notice.left().as_f32()).abs() <= 1.,
            "the menu's bottom-left corner is the notice's top-left corner: {menu:?} {notice:?}"
        );
        assert!(
            menu.bottom() <= notice.top() && menu.bottom() <= version.top(),
            "the menu covers neither the notice nor the version link: {menu:?} {notice:?} {version:?}"
        );
    });
}

#[gpui_kit::test]
fn check_now_toasts(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("0.0.5");
    set_update_check(cx, false);
    install_services(cx, env.services("0.1.0"));
    assert_eq!(env.manifest_requests(), 0, "automatic checks are off");

    // Up to date.
    cx.update(|cx| check_now(cx, CheckKind::Manual));
    cx.run_until_parked();
    assert!(has_toast(cx, &app, "GitCthulhu is up to date."));
    let state = snapshot_of(cx);
    assert!(!state.checking && state.notice.is_none() && state.last_checked.is_some());

    // A newer version.
    env.serve_manifest("9.9.9");
    cx.update(|cx| check_now(cx, CheckKind::Manual));
    cx.run_until_parked();
    assert!(has_toast(cx, &app, "GitCthulhu 9.9.9 is available."));
    assert_eq!(
        snapshot_of(cx).notice.map(|n| n.version),
        Some("9.9.9".to_owned())
    );

    // A failing manifest host: an error toast, the known notice stays, the page learns why.
    *env.manifest.lock().expect("lock") = (500, "oops".to_owned());
    cx.update(|cx| check_now(cx, CheckKind::Manual));
    cx.run_until_parked();
    assert!(has_toast(cx, &app, "Checking for updates failed:"));
    let state = snapshot_of(cx);
    assert!(state.last_error.is_some(), "the failure is remembered");
    assert_eq!(state.notice.map(|n| n.version), Some("9.9.9".to_owned()));
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::NOTICE).is_some());
    });

    // An automatic check that fails is silent.
    let before = cx.update(|cx| toast_texts(&app, cx)).len();
    cx.update(|cx| check_now(cx, CheckKind::Automatic));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| toast_texts(&app, cx)).len(), before);
}

#[gpui_kit::test]
fn interval_off(cx: &mut TestAppContext) {
    // Through the app: with the setting off no request is made; turning it on checks at once.
    init_app(cx);
    let (_handle, _app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("9.9.9");
    set_update_check(cx, false);
    install_services(cx, env.services("0.1.0"));
    assert_eq!(
        env.manifest_requests(),
        0,
        "nothing is asked while the setting is off"
    );
    assert_eq!(snapshot_of(cx).notice, None);
    set_update_check(cx, true);
    cx.run_until_parked();
    assert_eq!(
        env.manifest_requests(),
        1,
        "switching the setting on checks at once"
    );
    assert!(snapshot_of(cx).notice.is_some());
    // Switching another setting does not check again: the last check is fresh.
    cx.update(|cx| settings::update(cx, |s| s.date_24h = !s.date_24h));
    cx.run_until_parked();
    assert_eq!(env.manifest_requests(), 1);
}

#[gpui_kit::test]
fn disabled_without_manifest_url(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    let dir = tempfile::tempdir().expect("temp dir");
    let services = crate::backend::update::services_with_manifest(
        dir.path().join("data"),
        dir.path().join("cache"),
        None,
        "0.1.0",
    )
    .expect("services start");
    assert_eq!(
        crate::backend::update::check_now(&services, &crate::backend::CancelToken::new()),
        Err(UpdateCheckError::Disabled),
        "the service itself refuses without a manifest URL"
    );
    install_services(cx, services);
    cx.update(|cx| {
        assert!(!settings_available(cx), "no Updates page");
        assert!(!settings_dialog::is_available(cx, SettingsPage::Updates));
        assert!(
            !cx.default_global::<UpdateState>().scheduler_started,
            "no update task is started"
        );
        check_now(cx, CheckKind::Manual);
    });
    assert!(has_toast(cx, &app, "no update source configured"));
    let state = snapshot_of(cx);
    assert!(!state.checking && state.notice.is_none() && state.last_checked.is_none());
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::NOTICE).is_none());
    });
    // This build has no default endpoint either.
    #[cfg(feature = "services")]
    if option_env!("GITCTHULHU_UPDATE_MANIFEST_URL").is_none() {
        assert_eq!(crate::backend::update::manifest_url(), None);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// the Updates settings page
// ---------------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn updates_page_rows_and_actions(cx: &mut TestAppContext) {
    use crate::dialogs::settings::ids as sid;
    use crate::dialogs::settings::updates::ids as pid;

    init_app(cx);
    let (handle, app) = open_app(cx, crate::test_support::window_size(1280., 800.));
    let env = Env::new("9.9.9");
    set_update_check(cx, false);
    install_services(cx, env.services("0.1.0"));
    cx.update(|cx| assert!(settings_available(cx)));

    cx.update_window(handle, |_, window, cx| {
        settings_dialog::open(window, cx, Some(SettingsPage::Updates));
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(sid::entry(SettingsPage::Updates)).is_some(),
            "the category is listed"
        );
        let auto = bounds(window, sid::row(pid::AUTO));
        let check = bounds(window, sid::row(pid::CHECK));
        assert!(check.top() >= auto.bottom(), "rows follow each other");
        assert!(
            window.try_find(pid::CHANNEL_RELEASE).is_none(),
            "no channel picker without a nightly manifest URL"
        );
        let switch = bounds(window, sid::switch(pid::AUTO));
        let button = bounds(window, pid::CHECK_NOW);
        assert!(
            (switch.right().as_f32() - button.right().as_f32()).abs() <= 0.5,
            "the controls share the right edge: {switch:?} {button:?}"
        );
    })
    .expect("window");

    // The switch writes the setting.
    cx.update_window(handle, |_, window, cx| {
        window.click(sid::switch(pid::AUTO), cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert!(cx.update(|cx| settings::get_or_default(cx).update_check));
    assert_eq!(
        env.manifest_requests(),
        1,
        "switching it on checked at once"
    );

    // "Check now" runs a manual check and shows the result.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(pid::CHECK_NOW, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(env.manifest_requests(), 2);
    assert!(has_toast(cx, &app, "GitCthulhu 9.9.9 is available."));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let row = bounds(window, sid::row(pid::CHECK));
        assert!(
            row.size.height.as_f32() > 60.,
            "subtitle and last-checked note make the row tall: {row:?}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn gravatar_cached_and_fallback(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    let base = env.server.url("").trim_end_matches('/').to_owned();
    cx.update(|cx| cx.set_global(AvatarBase(base.clone())));
    let (window, _probe) = open_probe(cx, 200., 60., |_, _| {
        gpui_kit::div()
            .flex()
            .child(Avatar::new("Ilia", "ilia@example.com", 24.))
            .child(Avatar::new("Ilia again", " ILIA@example.com ", 24.))
            .child(Avatar::new("Other", "other@example.com", 24.))
            .into_any_element()
    });
    let redraw = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("window");
        cx.run_until_parked();
    };

    // No services yet: no fetcher, the initials show and nothing is requested.
    redraw(cx);
    assert_eq!(env.avatar_requests(), 0, "offline start: initials only");

    // The services start: the fetcher is installed and the avatars on screen are fetched, once per address.
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(env.services("0.1.0"));
    });
    redraw(cx);
    assert_eq!(
        env.avatar_requests(),
        2,
        "one request per distinct address: {:?}",
        env.server.requests()
    );
    redraw(cx);
    redraw(cx);
    assert_eq!(
        env.avatar_requests(),
        2,
        "the session cache answers repeated draws"
    );

    let cache = AvatarDiskCache::new(crate::paths::cache_dir().join("avatars"));
    let local = |email: &str| gravatar_url(email).replacen("https://www.gravatar.com", &base, 1);
    for email in ["ilia@example.com", "other@example.com"] {
        assert!(
            matches!(cache.lookup(&local(email), SystemTime::now()), Lookup::Image(bytes) if bytes == PNG.to_vec()),
            "the image of {email} went to the disk cache"
        );
    }
}

#[gpui_kit::test]
fn gravatar_missing_and_failing_addresses_fall_back(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    cx.update(|cx| {
        cx.set_global(AvatarBase(
            env.server.url("").trim_end_matches('/').to_owned(),
        ))
    });
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(env.services("0.1.0"));
    });
    cx.run_until_parked();
    let (window, _probe) = open_probe(cx, 100., 40., |_, _| {
        Avatar::new("Nobody", "nobody@example.com", 24.).into_any_element()
    });
    let draw = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("window");
        cx.run_until_parked();
    };
    *env.avatar.lock().expect("lock") = (404, Vec::new());
    draw(cx);
    draw(cx);
    assert_eq!(env.avatar_requests(), 1, "a 404 is asked once");
    let cache = AvatarDiskCache::new(crate::paths::cache_dir().join("avatars"));
    let url = gravatar_url("nobody@example.com").replacen(
        "https://www.gravatar.com",
        env.server.url("").trim_end_matches('/'),
        1,
    );
    assert_eq!(
        cache.lookup(&url, SystemTime::now()),
        Lookup::Absent,
        "the missing picture is remembered on disk"
    );

    // With avatars switched off nothing is requested at all.
    cx.update(|cx| settings::update(cx, |s| s.avatar_provider = AvatarProvider::None));
    draw(cx);
    assert_eq!(env.avatar_requests(), 1);
}

#[gpui_kit::test]
fn image_fetch_classifies_the_server_answer(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    let services = env.services("0.1.0");
    let url = env.server.url("/avatar/x");
    assert_eq!(
        crate::backend::update::fetch_image(&services, &url),
        ImageFetch::Found(PNG.to_vec())
    );
    *env.avatar.lock().expect("lock") = (404, Vec::new());
    assert_eq!(
        crate::backend::update::fetch_image(&services, &url),
        ImageFetch::NotFound
    );
    *env.avatar.lock().expect("lock") = (500, Vec::new());
    assert!(matches!(
        crate::backend::update::fetch_image(&services, &url),
        ImageFetch::Failed(_)
    ));
    assert!(matches!(
        crate::backend::update::fetch_image(&services, "ftp://example.com/a.png"),
        ImageFetch::Failed(_)
    ));
}

// ---------------------------------------------------------------------------------------------------------------
// review fixes: races, settings observer, offline retry, no real network in tests
// ---------------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn snooze_wins_over_a_check_that_was_running(cx: &mut TestAppContext) {
    init_app(cx);
    let (_handle, app) = open_app(cx, DEFAULT_SIZE);
    let found = UpdateNotice {
        version: "9.9.9".into(),
        url: "https://example.com/releases/v1".into(),
    };
    cx.update(|cx| {
        // A check started (snoozes_done = 0), then a snooze finished before the check's answer arrived.
        cx.default_global::<UpdateState>().snoozes_done = 1;
        finish_check(cx, CheckKind::Automatic, 0, 0, Ok(Some(found.clone())));
    });
    assert_eq!(snapshot_of(cx).notice, None, "the snooze stays in force");
    // A check that started after the snooze shows what it found.
    cx.update(|cx| finish_check(cx, CheckKind::Automatic, 1, 0, Ok(Some(found.clone()))));
    assert_eq!(snapshot_of(cx).notice, Some(found));

    // "Check now" while an automatic check runs: the running check reports in a toast.
    cx.update(|cx| {
        let state = cx.default_global::<UpdateState>();
        state.checking = true;
        state.manual_waiting = true;
        finish_check(cx, CheckKind::Automatic, 1, 0, Ok(None));
    });
    assert!(has_toast(cx, &app, "GitCthulhu is up to date."));
    assert!(!cx.update(|cx| cx.default_global::<UpdateState>().manual_waiting));
}

#[gpui_kit::test]
fn unrelated_settings_changes_do_not_recheck_after_a_failure(cx: &mut TestAppContext) {
    init_app(cx);
    let (_handle, _app) = open_app(cx, DEFAULT_SIZE);
    let env = Env::new("9.9.9");
    *env.manifest.lock().expect("lock") = (500, "down".to_owned());
    install_services(cx, env.services("0.1.0"));
    assert_eq!(
        env.manifest_requests(),
        1,
        "the startup check ran and failed"
    );
    assert!(snapshot_of(cx).last_error.is_some());
    for _ in 0..3 {
        cx.update(|cx| settings::update(cx, |s| s.date_24h = !s.date_24h));
        cx.run_until_parked();
    }
    assert_eq!(
        env.manifest_requests(),
        1,
        "typing in the settings does not start a check per change"
    );
    set_update_check(cx, false);
    cx.run_until_parked();
    set_update_check(cx, true);
    cx.run_until_parked();
    assert_eq!(
        env.manifest_requests(),
        2,
        "switching the setting on checks again"
    );
}

#[gpui_kit::test]
fn offline_avatars_are_asked_again_later(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    let base = env.server.url("").trim_end_matches('/').to_owned();
    cx.update(|cx| cx.set_global(AvatarBase(base)));
    set_update_check(cx, false);
    *env.avatar.lock().expect("lock") = (503, Vec::new());
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(env.services("0.1.0"));
    });
    cx.run_until_parked();
    let (window, _probe) = open_probe(cx, 100., 40., |_, _| {
        Avatar::new("Offline", "offline@example.com", 24.).into_any_element()
    });
    let draw = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("window");
        cx.run_until_parked();
    };
    draw(cx);
    draw(cx);
    assert_eq!(
        env.avatar_requests(),
        1,
        "offline: asked once, initials shown"
    );

    // Back online: after the retry delay the avatar is asked for again and lands in the disk cache.
    *env.avatar.lock().expect("lock") = (200, PNG.to_vec());
    cx.executor()
        .advance_clock(AVATAR_RETRY + Duration::from_secs(1));
    draw(cx);
    assert_eq!(env.avatar_requests(), 2, "the failed avatar is asked again");
    let cache = AvatarDiskCache::new(crate::paths::cache_dir().join("avatars"));
    let url = gravatar_url("offline@example.com").replacen(
        "https://www.gravatar.com",
        env.server.url("").trim_end_matches('/'),
        1,
    );
    assert!(matches!(
        cache.lookup(&url, SystemTime::now()),
        Lookup::Image(_)
    ));
}

#[gpui_kit::test]
fn offline_does_not_ask_for_every_avatar(cx: &mut TestAppContext) {
    // Many new authors while the network is down: after the first failures the queued avatars do not each wait for
    // the HTTP timeout; once the retry fires, all of them are asked for.
    const AUTHORS: usize = 10;
    init_app(cx);
    let env = Env::new("9.9.9");
    let base = env.server.url("").trim_end_matches('/').to_owned();
    cx.update(|cx| cx.set_global(AvatarBase(base.clone())));
    set_update_check(cx, false);
    *env.avatar.lock().expect("lock") = (503, Vec::new());
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(env.services("0.1.0"));
    });
    cx.run_until_parked();
    let email = |i: usize| format!("author{i}@offline.example.com");
    let (window, _probe) = open_probe(cx, 400., 40., move |_, _| {
        gpui_kit::div()
            .flex()
            .children((0..AUTHORS).map(|i| Avatar::new("A", email(i), 24.)))
            .into_any_element()
    });
    let draw = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("window");
        cx.run_until_parked();
    };
    draw(cx);
    let offline = env.avatar_requests();
    assert!(
        (1..=avatars::PARALLEL).contains(&offline),
        "at most the first batch reached the network: {offline}"
    );
    cx.update(|cx| {
        let gate = &cx.default_global::<UpdateState>().avatar_gate;
        assert_eq!(
            (gate.running(), gate.waiting()),
            (0, 0),
            "every place was given back"
        );
    });

    *env.avatar.lock().expect("lock") = (200, PNG.to_vec());
    cx.executor()
        .advance_clock(AVATAR_RETRY + Duration::from_secs(1));
    draw(cx);
    assert_eq!(
        env.avatar_requests(),
        offline + AUTHORS,
        "back online: every avatar is asked for once"
    );
    let cache = AvatarDiskCache::new(crate::paths::cache_dir().join("avatars"));
    for i in 0..AUTHORS {
        let url = gravatar_url(&email(i)).replacen("https://www.gravatar.com", &base, 1);
        assert!(matches!(
            cache.lookup(&url, SystemTime::now()),
            Lookup::Image(_)
        ));
    }
}

#[gpui_kit::test]
fn unit_tests_never_download_real_avatars(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(env.services("0.1.0"));
    });
    cx.run_until_parked();
    let fetcher = cx
        .update(|cx| cx.default_global::<UpdateState>().fetcher.clone())
        .expect("the fetcher is installed once the services run");
    let failed = Rc::new(RefCell::new(None));
    let sink = failed.clone();
    cx.update(|cx| {
        let task = fetcher(gravatar_url("someone@example.com"), cx);
        cx.spawn(async move |_| *sink.borrow_mut() = Some(task.await.is_err()))
            .detach();
    });
    cx.run_until_parked();
    assert_eq!(
        *failed.borrow(),
        Some(true),
        "no stand-in server: no download at all"
    );
    assert_eq!(env.avatar_requests(), 0);
}

// ---------------------------------------------------------------------------------------------------------------
// the channel and "Last checked"
// ---------------------------------------------------------------------------------------------------------------

impl Env {
    /// Services whose manifest URL has the `{channel}` placeholder, like a build with a nightly manifest.
    fn channel_services(&self, app_version: &str) -> ServicesHandle {
        crate::backend::update::services_with_manifest(
            self.dir.path().join("data"),
            self.dir.path().join("cache"),
            Some(&self.server.url("/manifest-{channel}.json")),
            app_version,
        )
        .expect("services start")
    }

    fn manifest_paths(&self) -> Vec<String> {
        self.server
            .requests()
            .into_iter()
            .filter(|p| p.starts_with("/manifest"))
            .collect()
    }
}

#[gpui_kit::test]
fn changing_the_channel_applies_at_once_and_checks_again(cx: &mut TestAppContext) {
    use crate::backend::update::set_nightly_channel_available_override;
    use crate::dialogs::settings::updates::{self, ids as pid};

    init_app(cx);
    let env = Env::new("9.9.9");
    cx.update(|_| set_nightly_channel_available_override(Some(true)));
    let (window, _probe) = open_probe(cx, 760., 400., |window, cx| updates::page(window, cx));
    install_services(cx, env.channel_services("0.1.0"));
    assert_eq!(
        env.manifest_paths(),
        ["/manifest-release.json"],
        "the startup check asks the release manifest"
    );
    assert_eq!(
        snapshot_of(cx).notice.map(|n| n.version),
        Some("9.9.9".to_owned())
    );

    // The nightly manifest announces another version; the click must reach it without a restart.
    env.serve_manifest("10.0.0");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(pid::CHANNEL_NIGHTLY, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        env.manifest_paths(),
        ["/manifest-release.json", "/manifest-nightly.json"],
        "choosing Nightly checks the nightly manifest at once"
    );
    let state = snapshot_of(cx);
    assert_eq!(
        state.notice.map(|n| n.version),
        Some("10.0.0".to_owned()),
        "the notice is the nightly channel's"
    );
    assert!(state.last_checked.is_some() && !state.checking);

    // Back to Release: asks the release manifest again (a repeated click on the chosen chip does nothing).
    cx.update_window(window, |_, window, cx| {
        window.click(pid::CHANNEL_NIGHTLY, cx);
        window.click(pid::CHANNEL_RELEASE, cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        env.manifest_paths().last().map(String::as_str),
        Some("/manifest-release.json")
    );
    assert_eq!(env.manifest_paths().len(), 3);
    cx.update(|_| set_nightly_channel_available_override(None));
}

#[gpui_kit::test]
fn the_last_check_time_comes_back_after_a_restart(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("0.1.0");
    set_update_check(cx, false);
    install_services(cx, env.services("0.1.0"));
    assert_eq!(
        snapshot_of(cx).last_checked,
        None,
        "nothing was ever checked"
    );
    cx.update(|cx| check_now(cx, CheckKind::Manual));
    cx.run_until_parked();
    let checked = snapshot_of(cx).last_checked.expect("the check finished");

    // A new session: no state, services over the same cache directory, no startup check.
    cx.update(|cx| cx.set_global(UpdateState::default()));
    install_services(cx, env.services("0.1.0"));
    cx.run_until_parked();
    let state = snapshot_of(cx);
    let stored = state.last_checked.expect("read from the cache");
    let drift = stored
        .duration_since(checked)
        .or_else(|err| Ok::<_, ()>(err.duration()))
        .expect("a duration");
    assert!(
        drift < Duration::from_secs(5),
        "the stored time is the check's: {stored:?} vs {checked:?}"
    );
    assert_eq!(
        env.manifest_requests(),
        1,
        "no check was needed to show the time"
    );
    assert!(!state.checking && state.notice.is_none());
}

#[gpui_kit::test]
fn a_check_running_during_a_channel_change_is_dropped_and_repeated(cx: &mut TestAppContext) {
    init_app(cx);
    let env = Env::new("9.9.9");
    set_update_check(cx, false);
    install_services(cx, env.channel_services("0.1.0"));
    assert!(env.manifest_paths().is_empty(), "automatic checks are off");

    // A check of the release channel is running (epoch 0) when the user picks Nightly.
    cx.update(|cx| {
        cx.default_global::<UpdateState>().checking = true;
        channel_changed(cx, settings::UpdateChannel::Nightly);
    });
    cx.run_until_parked();
    assert!(
        env.manifest_paths().is_empty(),
        "no second check while one is running"
    );
    let old = UpdateNotice {
        version: "1.0.0".to_owned(),
        url: "https://example.com/releases/old".to_owned(),
    };
    cx.update(|cx| finish_check(cx, CheckKind::Automatic, 0, 0, Ok(Some(old))));
    cx.run_until_parked();
    assert_eq!(
        env.manifest_paths(),
        ["/manifest-nightly.json"],
        "the release answer was dropped and the nightly manifest asked"
    );
    let state = snapshot_of(cx);
    assert_eq!(state.notice.map(|n| n.version), Some("9.9.9".to_owned()));
    assert!(!state.checking);

    // With automatic checks off and no check running, a change fetches nothing (until "Check now").
    cx.update(|cx| channel_changed(cx, settings::UpdateChannel::Release));
    cx.run_until_parked();
    assert_eq!(env.manifest_paths().len(), 1, "nothing was fetched");
    let state = snapshot_of(cx);
    assert!(state.notice.is_none() && state.last_checked.is_none());
}
