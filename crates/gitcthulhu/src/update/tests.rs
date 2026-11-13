use core::prelude::v1::test;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, AppContext as _, Global, TestAppContext, Window};

use super::avatars::{self, AvatarDiskCache, AvatarMiss, Lookup};
use super::notice;
use super::*;
use crate::backend::update::ImageFetch;
use crate::shell::platform::{self, ShellPlatform};
use crate::test_support::geometry::{bounds, open_probe};
use crate::test_support::init_app;

/// The 1x1 PNG a loopback "Gravatar" serves.
const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Redirects the avatar downloads of a test to a loopback server (the fetcher otherwise asks gravatar.com).
struct AvatarBase(String);

impl Global for AvatarBase {}

/// Called by the avatar fetcher in tests: the loopback URL, or `None` (no download at all) when the test set no
/// stand-in, so a unit test never reaches gravatar.com.
pub(super) fn redirect_avatar_url(cx: &App, url: String) -> Option<String> {
    cx.try_global::<AvatarBase>()
        .map(|base| url.replacen("https://www.gravatar.com", &base.0, 1))
}

/// What the loopback server answers: the manifest on `/manifest*`, the avatar on `/avatar/*`.
fn record_urls(cx: &mut TestAppContext) -> Rc<RefCell<Vec<String>>> {
    let urls = Rc::new(RefCell::new(Vec::new()));
    let sink = urls.clone();
    cx.update(|cx| {
        platform::install(
            cx,
            ShellPlatform::new(
                |_: &mut Window, _: &mut App, _| {},
                move |_, url| sink.borrow_mut().push(url.to_owned()),
            ),
        )
    });
    urls
}

#[gpui_kit::test]
fn notice_only_opens_https_links(cx: &mut TestAppContext) {
    init_app(cx);
    let urls = record_urls(cx);
    cx.update(|cx| {
        notice::open_download(cx, "http://example.com/a");
        notice::open_download(cx, "file:///c:/windows/system32/calc.exe");
        notice::open_download(cx, "https://example.com/b");
    });
    assert_eq!(*urls.borrow(), ["https://example.com/b".to_owned()]);
}

#[gpui_kit::test]
fn channel_picker_writes_the_setting(cx: &mut TestAppContext) {
    use crate::dialogs::settings::updates::{self, ids as pid};

    init_app(cx);
    let (window, _probe) = open_probe(cx, 760., 400., |_, cx| updates::content(cx, true));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let release = bounds(window, pid::CHANNEL_RELEASE);
        let nightly = bounds(window, pid::CHANNEL_NIGHTLY);
        assert!(
            (release.size.width.as_f32() - nightly.size.width.as_f32()).abs() <= 0.5
                && (release.size.width.as_f32() * 2. - 180.).abs() <= 1.,
            "two chips share the 180 dp dropdown width: {release:?} {nightly:?}"
        );
        assert!((nightly.left() - release.right()).as_f32().abs() <= 0.5);
        assert_eq!(release.size.height.as_f32(), 32.);
        window.click(pid::CHANNEL_NIGHTLY, cx);
    })
    .expect("window");
    assert_eq!(
        cx.update(|cx| settings::get_or_default(cx).update_channel),
        settings::UpdateChannel::Nightly
    );
    cx.update_window(window, |_, window, cx| {
        window.click(pid::CHANNEL_RELEASE, cx)
    })
    .expect("window");
    assert_eq!(
        cx.update(|cx| settings::get_or_default(cx).update_channel),
        settings::UpdateChannel::Release
    );
}

// ---------------------------------------------------------------------------------------------------------------
// avatars
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn gravatar_cache_policy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let cache = AvatarDiskCache::new(dir.path().join("avatars"));
    let now = SystemTime::now();
    let url = "https://www.gravatar.com/avatar/abc?s=80&d=404";
    let png = PNG.to_vec();

    assert_eq!(cache.lookup(url, now), Lookup::Miss);
    // First load: from the network, then from the cache without asking.
    let got = avatars::load(&cache, url, now, |_| ImageFetch::Found(png.clone()));
    assert_eq!(got, Ok(png.clone()));
    let got = avatars::load(&cache, url, now, |_| {
        panic!("a fresh image is not fetched again")
    });
    assert_eq!(got, Ok(png.clone()));

    // A week later the image is fetched again; offline, the stale image is used.
    let later = now + avatars::IMAGE_TTL + Duration::from_secs(60);
    assert_eq!(cache.lookup(url, later), Lookup::Miss);
    let got = avatars::load(&cache, url, later, |_| ImageFetch::Failed("offline".into()));
    assert_eq!(
        got,
        Ok(png),
        "offline: the stale image beats the initial letter"
    );
    let renewed = vec![1u8, 2, 3];
    let got = avatars::load(&cache, url, later, |_| ImageFetch::Found(renewed.clone()));
    assert_eq!(got, Ok(renewed), "online: the new image");

    // No picture for an address: remembered for a day, then asked again.
    let none = "https://www.gravatar.com/avatar/none?s=80&d=404";
    let got = avatars::load(&cache, none, now, |_| ImageFetch::NotFound);
    assert_eq!(got, Err(AvatarMiss::Absent));
    let got = avatars::load(&cache, none, now, |_| {
        panic!("a known absence is not asked again")
    });
    assert_eq!(got, Err(AvatarMiss::Absent));
    assert_eq!(cache.lookup(none, now), Lookup::Absent);
    assert_eq!(
        cache.lookup(none, now + avatars::ABSENT_TTL + Duration::from_secs(1)),
        Lookup::Miss
    );

    // A failure without anything cached is an error and is not remembered.
    let down = "https://www.gravatar.com/avatar/down?s=80&d=404";
    let got = avatars::load(&cache, down, now, |_| ImageFetch::Failed("offline".into()));
    assert_eq!(got, Err(AvatarMiss::Unreachable("offline".to_owned())));
    assert_eq!(cache.lookup(down, now), Lookup::Miss);

    // Pruning removes the oldest files beyond the limit.
    assert_eq!(cache.prune(1000), 0);
    assert_eq!(cache.prune(1), 1, "two files, limit one");
}

#[test]
fn avatar_downloads_are_limited_and_queued_in_order() {
    use futures::FutureExt as _;

    let mut gate = avatars::DownloadGate::new(2);
    assert!(
        gate.enter().is_none() && gate.enter().is_none(),
        "two run at once"
    );
    let mut third = gate.enter().expect("the third waits");
    let fourth = gate.enter().expect("the fourth waits");
    let mut fifth = gate.enter().expect("the fifth waits");
    assert_eq!((gate.running(), gate.waiting()), (2, 3));
    assert!(third.try_recv().expect("open").is_none());

    // A finished download hands its place to the first waiter.
    gate.leave();
    assert_eq!(third.now_or_never(), Some(Ok(())));
    assert_eq!((gate.running(), gate.waiting()), (2, 2));
    // A waiter that went away is skipped.
    drop(fourth);
    gate.leave();
    assert_eq!(fifth.try_recv(), Ok(Some(())));
    assert_eq!((gate.running(), gate.waiting()), (2, 0));
    gate.leave();
    gate.leave();
    assert_eq!(gate.running(), 0);
    gate.leave();
    assert_eq!(gate.running(), 0, "never below zero");
    assert!(avatars::DownloadGate::default().enter().is_none());
}

#[cfg(feature = "qa")]
#[gpui_kit::test]
fn qa_verbs_are_registered(cx: &mut TestAppContext) {
    init_app(cx);
    let described = cx.update(|cx| crate::qa::verbs::describe(cx));
    for verb in [
        "update serve",
        "update check",
        "update snooze",
        "open update menu",
        "assert update notice",
    ] {
        assert!(described.contains(verb), "{verb} is listed:\n{described}");
    }
}

#[test]
fn check_due_policy() {
    let now = SystemTime::now();
    let hours = |h: u64| now - Duration::from_secs(h * 3600);
    assert!(!check_due(false, None, now), "off: never");
    assert!(
        !check_due(false, Some(hours(100)), now),
        "off: never, however old"
    );
    assert!(check_due(true, None, now), "on and never checked: due");
    assert!(
        !check_due(true, Some(hours(23)), now),
        "checked 23 h ago: not yet"
    );
    assert!(
        check_due(true, Some(hours(24)), now),
        "checked 24 h ago: due"
    );
    assert!(
        !check_due(true, Some(now + Duration::from_secs(3600)), now),
        "a future time is not old"
    );
}

#[cfg(feature = "services")]
mod live;
