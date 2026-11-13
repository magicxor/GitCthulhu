use core::prelude::v1::test;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::badges::{self, ids as badge_ids, metrics as bm};
use super::poll::{Poller, Timings};
use super::{ActivityOverride, ForgeController, ForgeStatus, accounts, controller_of};
use crate::app_view::AppView;
use crate::backend::forge::testing::{
    FakeCheck, FakeForge, failing, passing, running, services_in,
};
use crate::backend::forge::{
    CheckCounts, CiState, DeviceCodeInfo, DevicePoll, Failure, FailureKind, NewAccount, PrState,
    ReviewBadge, add_account,
};
use crate::backend::{FullName, Sensitive};
use crate::dialogs::register::common::DialogHooks;
use crate::menus::{MenuItem, MenuKind, MenuTarget};
use crate::tab::RepoTab;
use crate::test_support::geometry::{assert_height, bounds};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, mount_fixture, mount_seeded, toast_texts,
};
use crate::theme::ActiveTokens as _;

// ---- pure ----------------------------------------------------------------------------------------------------

fn failure(kind: FailureKind) -> Failure {
    Failure {
        message: "boom".into(),
        kind,
    }
}

#[test]
fn poller_asks_every_tick_while_active_and_every_fifth_while_inactive() {
    let timings = Timings::default();
    let mut active = Poller::default();
    assert!(
        (0..5).all(|_| active.due(true, &timings)),
        "active: every tick"
    );
    let mut inactive = Poller::default();
    let due: Vec<bool> = (0..10).map(|_| inactive.due(false, &timings)).collect();
    assert_eq!(
        due,
        [
            false, false, false, false, true, false, false, false, false, true
        ],
        "inactive: every 5th tick"
    );
}

#[test]
fn poller_backs_off_after_failures_and_recovers() {
    let timings = Timings::default();
    let mut poller = Poller::default();
    assert!(poller.due(true, &timings));
    poller.failed(&failure(FailureKind::Offline), 0, &timings);
    assert!(poller.offline(), "an unreachable forge is offline");
    // 1st failure: wait 2 ticks; 2nd: 4; 3rd: 8; then the cap.
    let mut waits = Vec::new();
    for _ in 0..4 {
        let mut ticks = 0;
        while !poller.due(true, &timings) {
            ticks += 1;
            assert!(ticks < 100, "it asks again eventually");
        }
        waits.push(ticks + 1);
        poller.failed(&failure(FailureKind::Offline), 0, &timings);
    }
    assert_eq!(waits, [2, 4, 8, 8], "doubling back-off, capped at 8 ticks");
    poller.succeeded();
    assert!(poller.due(true, &timings), "a success clears the back-off");
    assert!(!poller.offline());
}

#[test]
fn poller_stops_after_errors_only_the_user_can_fix() {
    let timings = Timings::default();
    for kind in [
        FailureKind::Auth,
        FailureKind::Permission,
        FailureKind::NoAccount,
        FailureKind::NotAForge,
    ] {
        let mut poller = Poller::default();
        assert!(poller.due(true, &timings));
        poller.failed(&failure(kind), 0, &timings);
        assert!(poller.halt().is_some(), "{kind:?} halts polling");
        assert!(
            (0..20).all(|_| !poller.due(true, &timings)),
            "{kind:?}: no more refreshes until the user acts"
        );
        poller.user_acted();
        assert!(
            poller.due(true, &timings),
            "{kind:?}: asking again after the user acted"
        );
    }
}

#[test]
fn poller_waits_for_the_rate_limit_reset() {
    let timings = Timings::default();
    let mut poller = Poller::default();
    assert!(poller.due(true, &timings));
    poller.failed(
        &failure(FailureKind::RateLimited {
            reset_at: Some(150),
        }),
        0,
        &timings,
    );
    let mut ticks = 0;
    while !poller.due(true, &timings) {
        ticks += 1;
    }
    assert_eq!(ticks, 2, "150 s at 60 s per tick: 3 ticks later");
    assert!(poller.halt().is_none(), "a rate limit is not a stop");
}

fn badge(state: PrState, checks: CheckCounts) -> ReviewBadge {
    ReviewBadge {
        number: 123,
        symbol: "#".into(),
        state,
        title: Some("Add login".into()),
        checks,
        url: Some("https://ghe.invalid/acme/widgets/pull/123".into()),
        merged_head: None,
    }
}

#[test]
fn badge_colours_by_state() {
    let (tokens, light) = (
        crate::theme::builtin::dark(),
        crate::theme::builtin::light(),
    );
    for tokens in [tokens, light] {
        assert_eq!(badges::state_color(PrState::Open, &tokens), tokens.add_file);
        assert_eq!(
            badges::state_color(PrState::Draft, &tokens),
            tokens.on_background_secondary
        );
        assert_eq!(
            badges::state_color(PrState::Closed, &tokens),
            tokens.deleted_file
        );
        let merged = badges::state_color(PrState::Merged, &tokens);
        assert!(
            (merged.h - 0.75).abs() < 1e-6
                && merged.s == tokens.add_file.s
                && merged.l == tokens.add_file.l,
            "merged is add_file turned to purple"
        );
        assert_eq!(badges::ci_color(CiState::Passed, &tokens), tokens.add_file);
        assert_eq!(
            badges::ci_color(CiState::Running, &tokens),
            tokens.modified_file
        );
        assert_eq!(
            badges::ci_color(CiState::Failed, &tokens),
            tokens.deleted_file
        );
    }
}

#[test]
fn badge_tooltips_name_the_pull_request_its_state_and_the_checks() {
    let checks = CheckCounts {
        passed: 3,
        failed: 1,
        ..CheckCounts::default()
    };
    assert_eq!(
        badges::tooltip_text(&badge(PrState::Open, checks), false),
        "Pull request #123: Add login (open) · Checks: 3 passed, 1 failed"
    );
    assert_eq!(
        badges::tooltip_text(&badge(PrState::Merged, checks), false),
        "Pull request #123: Add login (merged)",
        "a merged review's checks no longer matter"
    );
    assert!(
        badges::tooltip_text(&badge(PrState::Draft, CheckCounts::default()), true)
            .ends_with("offline, showing the last known state")
    );
    let mut mr = badge(PrState::Open, CheckCounts::default());
    mr.symbol = "!".into();
    assert!(badges::tooltip_text(&mr, false).starts_with("Merge request !123"));
}

// ---- geometry of the badges ----------------------------------------------------------------------------------

fn full(name: &str) -> FullName {
    FullName::try_from(name.to_owned()).expect("a valid ref name")
}

fn local(name: &str, n: u64, is_head: bool) -> crate::backend::LocalBranch {
    crate::backend::LocalBranch {
        name: name.to_owned(),
        full: full(&format!("refs/heads/{name}")),
        target: crate::test_support::seed::oid(n),
        is_head,
        upstream: None,
        ahead_behind: None,
        checked_out_in: None,
    }
}

fn controller_in(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Entity<ForgeController> {
    let id = cx.update(|cx| tab.read(cx).id());
    cx.update(|cx| controller_of(id, cx))
        .expect("the tab has a forge controller")
}

#[gpui_kit::test]
fn badge_and_segment_geometry(cx: &mut TestAppContext) {
    let mut seed = RepoSeed::branchy();
    seed.refs.local = vec![
        local("feature", 11, false),
        local("main", 10, true),
        local("plain", 12, false),
    ];
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    let controller = controller_in(cx, &tab);
    cx.update(|cx| {
        controller.update(cx, |c, cx| {
            c.set_badges(
                [
                    (
                        full("refs/heads/feature"),
                        badge(
                            PrState::Open,
                            CheckCounts {
                                passed: 2,
                                ..CheckCounts::default()
                            },
                        ),
                    ),
                    (
                        full("refs/heads/main"),
                        badge(PrState::Draft, CheckCounts::default()),
                    ),
                ]
                .into_iter()
                .collect(),
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let pill = bounds(window, badge_ids::pill("refs/heads/feature"));
        assert_height(window, badge_ids::pill("refs/heads/feature"), bm::PILL_H);
        assert!(
            pill.size.width.as_f32() > 2. * bm::PILL_PAD_H,
            "the pill holds its padding and the number"
        );
        let dot = bounds(window, badge_ids::row_ci_dot("refs/heads/feature"));
        assert_eq!(
            (dot.size.width.as_f32(), dot.size.height.as_f32()),
            (bm::CI_DOT, bm::CI_DOT)
        );
        assert!(
            (dot.origin.x.as_f32()
                - (pill.origin.x.as_f32() + pill.size.width.as_f32() + bm::CI_DOT_MARGIN_START))
                .abs()
                < 0.5,
            "the CI dot starts 6 dp after the pill"
        );
        let pill_mid = pill.origin.y.as_f32() + pill.size.height.as_f32() / 2.;
        let dot_mid = dot.origin.y.as_f32() + dot.size.height.as_f32() / 2.;
        assert!(
            (pill_mid - dot_mid).abs() < 0.5,
            "pill and dot are centred on one line"
        );
        // The draft review of the checked-out branch: a pill, no dot (no checks), and "HEAD" follows.
        assert!(
            window
                .try_find(badge_ids::row_ci_dot("refs/heads/main"))
                .is_none(),
            "no checks, no dot"
        );
        let row = bounds(window, crate::side_panel::ids::branch("refs/heads/main"));
        let head = bounds(
            window,
            crate::side_panel::ids::child(
                crate::side_panel::ids::branch("refs/heads/main"),
                "head",
            ),
        );
        let badge_box = bounds(window, badge_ids::row_badge("refs/heads/main"));
        assert!(
            (head.origin.x.as_f32()
                - (badge_box.origin.x.as_f32()
                    + badge_box.size.width.as_f32()
                    + bm::ROW_MARGIN_END))
                .abs()
                < 0.5,
            "8 dp between the badge and HEAD"
        );
        assert!(row.size.width.as_f32() > 0.);
        // The addition does not move Gitnuro's layout: a row with a badge is as high as one without.
        let plain = bounds(window, crate::side_panel::ids::branch("refs/heads/plain"));
        let feature = bounds(window, crate::side_panel::ids::branch("refs/heads/feature"));
        assert_eq!(row.size.height, plain.size.height);
        assert_eq!(feature.size.height, plain.size.height);
        assert_eq!(row.size.width, plain.size.width);
    })
    .expect("window");
}

/// The badge sizes are the literal values of 21 section 5.5 (the geometry tests compare against these constants).
#[test]
fn badge_metrics_are_the_spec_values() {
    assert_eq!(
        (
            bm::PILL_H,
            bm::PILL_RADIUS,
            bm::PILL_PAD_H,
            bm::PILL_BG_ALPHA
        ),
        (18., 9., 6., 0.2)
    );
    assert_eq!(
        (bm::CI_DOT, bm::CI_DOT_MARGIN_START, bm::ROW_MARGIN_END),
        (8., 6., 8.)
    );
    assert_eq!(
        (
            bm::SEGMENT_H,
            bm::SEGMENT_PAD_H,
            bm::SEGMENT_DOT_BORDER,
            bm::SEGMENT_DOT_GAP
        ),
        (26., 6., 1., 4.)
    );
    assert_eq!(crate::dialogs::pull_request::DESCRIPTION_H, 200.);
    assert_eq!(crate::dialogs::pull_request::BASE_W, 240.);
    assert_eq!(crate::dialogs::settings::accounts::AVATAR_SIZE, 32.);
}

#[gpui_kit::test]
fn chip_segment_is_a_chip_high_with_an_on_primary_border_dot(cx: &mut TestAppContext) {
    let (handle, _probe, _controller) = {
        let ctx = cx.update(crate::test_support::inert_tab_ctx);
        let controller = cx
            .update(|cx| super::create_controller(&ctx, cx))
            .expect("a controller");
        cx.update(|cx| {
            controller.update(cx, |c, cx| {
                c.set_badges(
                    [(
                        full("refs/heads/feature"),
                        badge(
                            PrState::Open,
                            CheckCounts {
                                failed: 1,
                                ..CheckCounts::default()
                            },
                        ),
                    )]
                    .into_iter()
                    .collect(),
                    cx,
                )
            })
        });
        crate::test_support::init_app(cx);
        let (handle, probe) =
            crate::test_support::geometry::open_probe(cx, 300., 60., move |_, cx| {
                gpui_kit::IntoElement::into_any_element(gpui_kit::ParentElement::child(
                    gpui_kit::Styled::mt(gpui_kit::div(), gpui_kit::px(10.)),
                    badges::chip_segment(&ctx, &full("refs/heads/feature"), cx).expect("a segment"),
                ))
            });
        (handle, probe, controller)
    };
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(
            window,
            badge_ids::segment("refs/heads/feature"),
            bm::SEGMENT_H,
        );
        let dot = bounds(window, badge_ids::segment_ci_dot("refs/heads/feature"));
        assert_eq!(dot.size.width.as_f32(), bm::CI_DOT);
        let tokens = *cx.tokens();
        assert_eq!(
            tokens.deleted_file,
            badges::ci_color(CiState::Failed, &tokens)
        );
    })
    .expect("window");
}

/// 21 §5.5 / 08 §2.12: the PR segment is the right cap of its chip: its background rounds the right end with the
/// 16 dp chip radius (the chip's clip is rectangular).
#[test]
fn chip_segment_rounds_the_right_end() {
    let mut shell = badges::segment_shell(gpui_kit::Hsla::default());
    let r = gpui_kit::Styled::style(&mut shell).corner_radii.clone();
    let dp = |c: Option<gpui_kit::AbsoluteLength>| match c {
        Some(gpui_kit::AbsoluteLength::Rems(rems)) => Some(rems.0 * crate::metrics::BASE_REM),
        other => panic!("unexpected corner {other:?}"),
    };
    assert!((dp(r.top_right).unwrap() - 16.).abs() < 1e-3);
    assert!((dp(r.bottom_right).unwrap() - 16.).abs() < 1e-3);
    assert!(
        r.top_left.is_none() && r.bottom_left.is_none(),
        "the left end stays square"
    );
}

// ---- the whole path against the fake forge -------------------------------------------------------------------

struct World {
    handle: AnyWindowHandle,
    app: Entity<AppView>,
    tab: Entity<RepoTab>,
    controller: Entity<ForgeController>,
    fake: FakeForge,
    fixture: Fixture,
    tip: String,
    _tmp: tempfile::TempDir,
}

fn settle(cx: &mut TestAppContext, controller: &Entity<ForgeController>) {
    for _ in 0..200 {
        cx.run_until_parked();
        if !cx.update(|cx| controller.read(cx).is_busy()) {
            break;
        }
        cx.executor().advance_clock(Duration::from_millis(10));
    }
    cx.run_until_parked();
}

/// A repository whose origin is the fake forge (pushes go to a local bare repository), with a branch tracking
/// origin for each of `prs` (and a pull request on the fake forge), a branch `newfeat` with a commit and no upstream.
fn world(cx: &mut TestAppContext, prs: &[(&str, bool, Vec<FakeCheck>)], sign_in: bool) -> World {
    let fake = FakeForge::start();
    let fixture = Fixture::new();
    fixture.write("README.md", "hello\n");
    fixture.commit_all("Initial commit");
    let tip = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    fixture.git(&["remote", "add", "origin", &fake.server.remote_url()]);
    let bare = fixture.dir.path().join("bare.git");
    let bare_text = bare.to_string_lossy().into_owned();
    fixture.git(&["init", "-q", "--bare", &bare_text]);
    fixture.git(&["remote", "set-url", "--push", "origin", &bare_text]);
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fixture.git(&["config", "branch.main.remote", "origin"]);
    fixture.git(&["config", "branch.main.merge", "refs/heads/main"]);
    for (branch, draft, checks) in prs {
        fixture.git(&["branch", branch]);
        fixture.git(&[
            "update-ref",
            &format!("refs/remotes/origin/{branch}"),
            "HEAD",
        ]);
        fixture.git(&["config", &format!("branch.{branch}.remote"), "origin"]);
        fixture.git(&[
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        ]);
        fake.add(
            branch,
            &format!("PR for {branch}"),
            *draft,
            &tip,
            checks.clone(),
        );
    }
    fixture.git(&["checkout", "-q", "-b", "newfeat"]);
    fixture.write("feature.txt", "feature\n");
    fixture.commit_all("Add the feature\n\nIt does things.");
    fixture.git(&["checkout", "-q", "main"]);
    let (handle, app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    cx.update(|cx| cx.set_global(ActivityOverride(Rc::new(Cell::new(true)))));
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), None);
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("the app state").services = Some(services.clone());
    });
    let controller = controller_in(cx, &tab);
    if sign_in {
        sign_in_to(cx, &fake, &services);
    } else {
        // The services are there now: the loop finds them at its next look.
        cx.executor().advance_clock(Duration::from_millis(600));
    }
    settle(cx, &controller);
    World {
        handle,
        app,
        tab,
        controller,
        fake,
        fixture,
        tip,
        _tmp: tmp,
    }
}

fn sign_in_to(
    cx: &mut TestAppContext,
    fake: &FakeForge,
    services: &crate::backend::services::ServicesHandle,
) {
    add_account(
        services,
        NewAccount::GitHubEnterprise {
            api_url: fake.server.api_url(),
            token: Sensitive::new("secret-token".into()),
        },
    )
    .expect("sign in to the fake forge");
    cx.update(|cx| {
        accounts::installed(cx)
            .expect("the accounts model")
            .update(cx, |model, cx| model.added(cx));
    });
}

fn badge_of(cx: &mut TestAppContext, world: &World, branch: &str) -> Option<ReviewBadge> {
    cx.update(|cx| {
        world
            .controller
            .read(cx)
            .badge(&full(&format!("refs/heads/{branch}")))
            .cloned()
    })
}

#[gpui_kit::test]
fn badge_map_computed_in_background(cx: &mut TestAppContext) {
    let w = world(
        cx,
        &[
            ("feature", false, passing()),
            ("topic", true, running()),
            ("fix", false, failing()),
        ],
        false,
    );
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::NoForge,
        "a self-hosted host is a forge only once an account names it"
    );
    assert!(cx.update(|cx| w.controller.read(cx).badges().is_empty()));
    assert_eq!(
        w.fake.server.count(|r| r.url.contains("/pulls")),
        0,
        "no account, no request"
    );
    let services = cx.update(|cx| crate::app::services(cx)).expect("services");
    sign_in_to(cx, &w.fake, &services);
    settle(cx, &w.controller);
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::Idle
    );
    let feature = badge_of(cx, &w, "feature").expect("feature has a pull request");
    assert_eq!(
        (feature.state, feature.checks.passed, feature.checks.failed),
        (PrState::Open, 2, 0)
    );
    assert_eq!(feature.title.as_deref(), Some("PR for feature"));
    assert_eq!(
        feature.url.as_deref(),
        Some(format!("{}/acme/widgets/pull/1", w.fake.server.url()).as_str()),
        "the badge links to the review"
    );
    let topic = badge_of(cx, &w, "topic").expect("topic has a draft");
    assert_eq!(topic.state, PrState::Draft);
    assert_eq!(topic.checks.state(), Some(CiState::Running));
    let fix = badge_of(cx, &w, "fix").expect("fix has a pull request");
    assert_eq!(fix.checks.state(), Some(CiState::Failed));
    assert!(
        badge_of(cx, &w, "newfeat").is_none(),
        "a branch without a pull request has no badge"
    );
    assert!(badge_of(cx, &w, "main").is_none());
    // The remote-tracking branch of a reviewed branch has the badge too (its menu uses it).
    let tracking = cx.update(|cx| {
        w.controller
            .read(cx)
            .badge(&full("refs/remotes/origin/feature"))
            .map(|b| b.number)
    });
    assert_eq!(tracking, Some(1));
    // The side panel row shows it, from the map (no request at render time).
    for request in w
        .fake
        .server
        .requests()
        .iter()
        .filter(|r| r.url.contains("/pulls"))
    {
        assert_eq!(
            request.auth.as_deref(),
            Some("Bearer secret-token"),
            "every API request carries the stored token"
        );
    }
    let requests = w.fake.server.requests().len();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(window, badge_ids::pill("refs/heads/feature"), bm::PILL_H);
    })
    .expect("window");
    assert_eq!(
        w.fake.server.requests().len(),
        requests,
        "rendering never calls the forge"
    );
}

fn items_of(
    cx: &mut TestAppContext,
    w: &World,
    kind: MenuKind,
    branch: Option<&str>,
    remote: bool,
    commit: Option<&str>,
) -> Vec<String> {
    let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
    let target = MenuTarget {
        kind,
        tab: ctx,
        branch: branch.map(|b| {
            full(&if remote {
                format!("refs/remotes/{b}")
            } else {
                format!("refs/heads/{b}")
            })
        }),
        upstream: None,
        commit: commit.map(|c| c.parse().expect("an object id")),
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    };
    cx.update(|cx| {
        let items = match kind {
            MenuKind::LocalBranch => crate::menus::refs::local_branch(&target, cx),
            MenuKind::RemoteBranch => crate::menus::refs::remote_branch(&target, cx),
            _ => crate::menus::log::commit(&target, cx),
        };
        items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { label, .. } => Some(label.to_string()),
                MenuItem::Separator => None,
            })
            .collect()
    })
}

#[gpui_kit::test]
fn menu_items_when_forge_detected(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    let local = items_of(
        cx,
        &w,
        MenuKind::LocalBranch,
        Some("feature"),
        false,
        Some(&w.tip),
    );
    assert!(
        local.contains(&"Open pull request #1".to_owned()),
        "a branch with a pull request: {local:?}"
    );
    assert!(!local.iter().any(|l| l.starts_with("Create pull request")));
    let none = items_of(
        cx,
        &w,
        MenuKind::LocalBranch,
        Some("newfeat"),
        false,
        Some(&w.tip),
    );
    assert!(
        none.contains(&"Create pull request…".to_owned()),
        "a branch without one: {none:?}"
    );
    let remote = items_of(
        cx,
        &w,
        MenuKind::RemoteBranch,
        Some("origin/feature"),
        true,
        Some(&w.tip),
    );
    assert!(
        remote.contains(&"Open pull request #1".to_owned()),
        "{remote:?}"
    );
    assert!(
        remote.contains(&"Checkout pull request #1".to_owned()),
        "{remote:?}"
    );
    let commit = items_of(cx, &w, MenuKind::Commit, None, false, Some(&w.tip));
    assert!(
        commit.contains(&"Open commit in browser".to_owned()),
        "{commit:?}"
    );
    // Open pull request opens the review URL through the platform hook.
    let opened = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = opened.clone();
    cx.update(|cx| {
        cx.set_global(DialogHooks {
            open_url: Some(Rc::new(move |url| sink.borrow_mut().push(url.to_owned()))),
            ..DialogHooks::default()
        });
    });
    let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
    let target = MenuTarget {
        kind: MenuKind::LocalBranch,
        tab: ctx,
        branch: Some(full("refs/heads/feature")),
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    };
    let items = cx.update(|cx| {
        let mut items = Vec::new();
        super::menus::local_branch(&target, &mut items, cx);
        items
    });
    cx.update_window(w.handle, |_, window, cx| {
        for item in &items {
            if let MenuItem::Action { id, on_click, .. } = item
                && id.as_ref() == super::menus::ids::OPEN_PR
            {
                on_click(window, cx);
            }
        }
    })
    .expect("window");
    assert_eq!(
        opened.borrow().as_slice(),
        [format!("{}/acme/widgets/pull/1", w.fake.server.url())]
    );
}

#[gpui_kit::test]
fn remote_branch_without_a_review_offers_create_without_a_push(cx: &mut TestAppContext) {
    use crate::dialogs::pull_request::{self, ids};
    let w = world(cx, &[("feature", false, passing())], true);
    // A branch that is on the push remote but has no review yet, and the remote's default branch.
    w.fixture.git(&[
        "update-ref",
        "refs/remotes/origin/side",
        "refs/heads/newfeat",
    ]);
    w.fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    cx.update(|cx| {
        w.tab
            .update(cx, |t, cx| t.reload(crate::backend::RefreshSet::ALL, cx))
    });
    settle(cx, &w.controller);
    let side = items_of(
        cx,
        &w,
        MenuKind::RemoteBranch,
        Some("origin/side"),
        true,
        None,
    );
    assert!(
        side.contains(&"Create pull request…".to_owned()),
        "a remote branch without a review: {side:?}"
    );
    assert!(!side.iter().any(|l| l.starts_with("Checkout pull request")));
    let main = items_of(
        cx,
        &w,
        MenuKind::RemoteBranch,
        Some("origin/main"),
        true,
        None,
    );
    assert!(
        !main.iter().any(|l| l.starts_with("Create pull request")),
        "never from the remote's default branch: {main:?}"
    );
    let feature = items_of(
        cx,
        &w,
        MenuKind::RemoteBranch,
        Some("origin/feature"),
        true,
        None,
    );
    assert!(
        !feature.iter().any(|l| l.starts_with("Create pull request")),
        "a branch with a review opens it instead: {feature:?}"
    );
    // The dialog for the remote branch: nothing to push, the head is the remote branch.
    let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
    cx.update_window(w.handle, |_, window, cx| {
        pull_request::open(window, cx, ctx.clone(), full("refs/remotes/origin/side"));
    })
    .expect("window");
    settle(cx, &w.controller);
    cx.run_until_parked();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(ids::PUSH).is_none(), "no push option");
    })
    .expect("window");
    let shot = cx.update(|cx| {
        cx.global::<pull_request::OpenDialog>()
            .0
            .upgrade()
            .expect("the dialog is open")
            .read(cx)
            .snapshot(cx)
    });
    assert_eq!(shot.head, "acme:side");
    assert!(!shot.shows_push);
    assert_eq!(shot.title, "Add the feature");
}

#[gpui_kit::test]
fn no_forge_items_without_a_forge(cx: &mut TestAppContext) {
    // A repository whose origin is a local path: no forge, no items, no controller work.
    let fixture = Fixture::new();
    fixture.write("README.md", "hello\n");
    fixture.commit_all("Initial commit");
    fixture.git(&["remote", "add", "origin", "/srv/git/plain.git"]);
    fixture.git(&["branch", "topic"]);
    let (_handle, _app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    cx.update(|cx| cx.set_global(ActivityOverride(Rc::new(Cell::new(true)))));
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), None);
    cx.update(|cx| {
        crate::app::state_mut(cx).expect("state").services = Some(services);
    });
    let controller = controller_in(cx, &tab);
    cx.executor().advance_clock(Duration::from_millis(600));
    settle(cx, &controller);
    assert_eq!(
        cx.update(|cx| controller.read(cx).status().clone()),
        ForgeStatus::NoForge
    );
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let target = MenuTarget {
        kind: MenuKind::LocalBranch,
        tab: ctx,
        branch: Some(full("refs/heads/topic")),
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    };
    let items = cx.update(|cx| {
        let mut items = Vec::new();
        super::menus::local_branch(&target, &mut items, cx);
        items
    });
    assert!(items.is_empty(), "no forge, no forge items");
}

#[gpui_kit::test]
fn a_known_forge_without_an_account_asks_nothing_and_explains(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture.write(
        "README.md",
        "hello
",
    );
    fixture.commit_all("Initial commit");
    fixture.git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widgets.git",
    ]);
    fixture.git(&["branch", "topic"]);
    let (handle, app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    cx.update(|cx| cx.set_global(ActivityOverride(Rc::new(Cell::new(true)))));
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), None);
    cx.update(|cx| crate::app::state_mut(cx).expect("state").services = Some(services));
    let controller = controller_in(cx, &tab);
    cx.executor().advance_clock(Duration::from_millis(600));
    settle(cx, &controller);
    assert_eq!(
        cx.update(|cx| controller.read(cx).status().clone()),
        ForgeStatus::NoAccount
    );
    assert!(!cx.update(|cx| controller.read(cx).can_use_reviews()));
    assert!(cx.update(|cx| controller.read(cx).badges().is_empty()));
    // "Create pull request…" is there; without an account it explains instead of opening the dialog.
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let target = MenuTarget {
        kind: MenuKind::LocalBranch,
        tab: ctx,
        branch: Some(full("refs/heads/topic")),
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    };
    let items = cx.update(|cx| {
        let mut items = Vec::new();
        super::menus::local_branch(&target, &mut items, cx);
        items
    });
    cx.update_window(handle, |_, window, cx| {
        for item in &items {
            if let MenuItem::Action { id, on_click, .. } = item
                && id.as_ref() == super::menus::ids::CREATE_PR
            {
                on_click(window, cx);
            }
        }
    })
    .expect("window");
    cx.run_until_parked();
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts
            .iter()
            .any(|t| t == "Connect a GitHub account to use pull requests"),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn polling_cadence_and_stop_on_terminal_error(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    // The cadence starts after detection: one tick per minute.
    let tick = Duration::from_secs(60);
    let before = cx.update(|cx| w.controller.read(cx).refresh_count());
    for _ in 0..3 {
        cx.executor().advance_clock(tick);
        settle(cx, &w.controller);
    }
    let after = cx.update(|cx| w.controller.read(cx).refresh_count());
    assert!(
        after >= before + 3,
        "three minutes, at least three refreshes ({before} -> {after})"
    );
    // The token is rejected from now on: one failing refresh, then silence until the user acts.
    w.fake.server.set_unauthorized(true);
    cx.executor().advance_clock(tick);
    settle(cx, &w.controller);
    assert!(
        matches!(
            cx.update(|cx| w.controller.read(cx).status().clone()),
            ForgeStatus::AuthFailed(_)
        ),
        "a rejected token stops polling"
    );
    let asked = w.fake.server.count(|r| r.url.contains("/pulls"));
    for _ in 0..5 {
        cx.executor().advance_clock(tick);
        settle(cx, &w.controller);
    }
    assert_eq!(
        w.fake.server.count(|r| r.url.contains("/pulls")),
        asked,
        "no request after a terminal error"
    );
    assert!(
        badge_of(cx, &w, "feature").is_some(),
        "the badges from the cache stay"
    );
    // The user acts (a refresh from the menu, or an account change): polling resumes.
    w.fake.server.set_unauthorized(false);
    cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
    settle(cx, &w.controller);
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::Idle
    );
}

#[gpui_kit::test]
fn an_account_change_after_a_rejected_token_refreshes_at_once(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    w.fake.server.set_unauthorized(true);
    cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
    settle(cx, &w.controller);
    assert!(matches!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::AuthFailed(_)
    ));
    // The user replaces the token: the account list changes, the facts (same host, same label) do not.
    w.fake.server.set_unauthorized(false);
    let asked = w.fake.server.count(|r| r.url.contains("/pulls"));
    cx.update(|cx| {
        accounts::installed(cx)
            .expect("the accounts model")
            .update(cx, |model, cx| model.added(cx));
    });
    settle(cx, &w.controller);
    assert!(
        w.fake.server.count(|r| r.url.contains("/pulls")) > asked,
        "the forge is asked right away, not a tick later"
    );
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::Idle
    );
}

#[gpui_kit::test]
fn a_refresh_requested_during_a_refresh_runs_after_it(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    let before = cx.update(|cx| w.controller.read(cx).refresh_count());
    // Two requests back to back (the second arrives while the first runs): both run.
    cx.update(|cx| {
        w.controller.update(cx, |c, cx| {
            c.refresh_now(cx);
            c.refresh_now(cx);
        })
    });
    settle(cx, &w.controller);
    let after = cx.update(|cx| w.controller.read(cx).refresh_count());
    assert_eq!(after - before, 2, "the second request is not dropped");
}

#[gpui_kit::test]
fn forgetting_the_account_removes_the_cached_badges(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    assert!(badge_of(cx, &w, "feature").is_some(), "signed in: a badge");
    let services = cx.update(|cx| crate::app::services(cx)).expect("services");
    for account in crate::backend::forge::accounts(&services).expect("the accounts") {
        crate::backend::forge::forget_account(&services, &account.id).expect("forget");
    }
    cx.update(|cx| {
        accounts::installed(cx)
            .expect("the accounts model")
            .update(cx, |model, cx| model.added(cx));
    });
    settle(cx, &w.controller);
    assert!(
        cx.update(|cx| w.controller.read(cx).badges().is_empty()),
        "no account, no badges"
    );
    assert!(badge_of(cx, &w, "feature").is_none());
}

#[gpui_kit::test]
fn inactive_window_polls_every_fifth_tick(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    let active = cx.update(|cx| cx.global::<ActivityOverride>().0.clone());
    active.set(false);
    let before = cx.update(|cx| w.controller.read(cx).refresh_count());
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_secs(60));
        settle(cx, &w.controller);
    }
    let after = cx.update(|cx| w.controller.read(cx).refresh_count());
    assert_eq!(after - before, 2, "ten ticks while inactive: two refreshes");
}

#[gpui_kit::test]
fn offline_keeps_the_cached_badges_and_backs_off(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    assert!(badge_of(cx, &w, "feature").is_some());
    w.fake.server.set_offline(true);
    cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
    settle(cx, &w.controller);
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::Offline
    );
    assert!(cx.update(|cx| w.controller.read(cx).offline()));
    let kept = badge_of(cx, &w, "feature").expect("the badge from the cache stays while offline");
    assert_eq!(kept.state, PrState::Open);
    // The tooltip says so.
    assert!(badges::tooltip_text(&kept, true).contains("offline"));
    // Back-off: the next tick does not ask (2 ticks), the one after does.
    let asked = w.fake.server.count(|r| r.url.contains("/pulls"));
    w.fake.server.set_offline(false);
    cx.executor().advance_clock(Duration::from_secs(60));
    settle(cx, &w.controller);
    assert_eq!(
        w.fake.server.count(|r| r.url.contains("/pulls")),
        asked,
        "backing off"
    );
    cx.executor().advance_clock(Duration::from_secs(60));
    settle(cx, &w.controller);
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::Idle,
        "reachable again"
    );
}

#[gpui_kit::test]
fn a_merged_review_shows_only_on_the_branch_that_landed(cx: &mut TestAppContext) {
    let w = world(cx, &[("feature", false, passing())], true);
    w.fake.merge("feature");
    cx.update(|cx| w.controller.update(cx, |c, cx| c.refresh_now(cx)));
    settle(cx, &w.controller);

    let merged = badge_of(cx, &w, "feature").expect("merged review at the same tip");
    assert_eq!(merged.state, PrState::Merged);
    assert_eq!(merged.checks, CheckCounts::default(), "merged: no checks");
    // The branch moves on: the old merge is not its review any more.
    w.fixture.git(&["checkout", "-q", "feature"]);
    w.fixture.write("more.txt", "more\n");
    w.fixture.commit_all("More work");
    w.fixture.git(&["checkout", "-q", "main"]);
    cx.update(|cx| {
        w.tab
            .update(cx, |t, cx| t.reload(crate::backend::RefreshSet::ALL, cx))
    });
    settle(cx, &w.controller);
    assert!(
        badge_of(cx, &w, "feature").is_none(),
        "a branch beyond the merged tip has no merged badge"
    );
}

#[gpui_kit::test]
fn create_pr_dialog_fields_and_push_first(cx: &mut TestAppContext) {
    use crate::dialogs::pull_request::{self, ids};
    let w = world(cx, &[("feature", false, passing())], true);
    let ctx = cx.update(|cx| w.tab.read(cx).ctx().clone());
    cx.update_window(w.handle, |_, window, cx| {
        pull_request::open(window, cx, ctx.clone(), full("refs/heads/newfeat"));
    })
    .expect("window");
    settle(cx, &w.controller);
    cx.run_until_parked();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        for id in [
            ids::TITLE,
            ids::DESCRIPTION,
            ids::HEAD,
            ids::DRAFT,
            ids::PUSH,
            ids::CREATE,
            ids::CANCEL,
        ] {
            assert!(window.try_find(id).is_some(), "the dialog shows {id}");
        }
        let dialog = bounds(window, ids::DIALOG);
        assert_eq!(dialog.size.width.as_f32(), 720., "Clone dialog geometry");
        assert_height(window, ids::DESCRIPTION, pull_request::DESCRIPTION_H);
    })
    .expect("window");
    let shot = cx.update(|cx| {
        cx.global::<pull_request::OpenDialog>()
            .0
            .upgrade()
            .expect("the dialog is open")
            .read(cx)
            .snapshot(cx)
    });
    // One commit: its subject is the title, the rest the description, the head names the owner.
    assert_eq!(shot.title, "Add the feature");
    assert_eq!(shot.body, "It does things.");
    assert_eq!(shot.head, "acme:newfeat");
    assert_eq!(shot.base, "main", "the remote's default branch is the base");
    assert!(shot.base_options.contains(&"main".to_owned()));
    assert!(
        shot.shows_push && shot.push,
        "no upstream: push first, checked"
    );
    assert!(!shot.draft);
    // Create: the branch is pushed first (a tab operation), then the review is created on the fake forge.
    cx.update_window(w.handle, |_, window, cx| {
        window.click(ids::CREATE, cx);
    })
    .expect("window");
    for _ in 0..50 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    cx.run_until_parked();
    let bare = w.fixture.dir.path().join("bare.git");
    let pushed = w.fixture.git(&[
        "--git-dir",
        &bare.to_string_lossy(),
        "rev-parse",
        "refs/heads/newfeat",
    ]);
    assert!(
        !pushed.trim().is_empty(),
        "the branch reached the remote before the review was created"
    );
    let created = w
        .fake
        .prs()
        .into_iter()
        .find(|pr| pr.branch == "newfeat")
        .expect("the review was created");
    assert_eq!(created.title, "Add the feature");
    assert!(!created.draft);
    let toasts = cx.update(|cx| toast_texts(&w.app, cx));
    assert!(
        toasts.iter().any(|t| t == "Pull request #2 created"),
        "the success toast: {toasts:?}"
    );
}

// ---- the Accounts page and the sign-in dialogs -------------------------------------------------------------

fn open_accounts_page(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        crate::dialogs::settings::open(
            window,
            cx,
            Some(crate::dialogs::settings::SettingsPage::Accounts),
        );
    })
    .expect("window");
    for _ in 0..20 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    cx.run_until_parked();
}

fn dialog_open(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    use gpui_kit::component::WindowExt as _;
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .expect("window")
}

#[gpui_kit::test]
fn accounts_page_layout(cx: &mut TestAppContext) {
    use crate::backend::forge::Forge;
    use crate::dialogs::settings::accounts::{AVATAR_SIZE, ids};
    let w = world(cx, &[], true);
    open_accounts_page(cx, w.handle);
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        for id in [
            crate::dialogs::settings::ids::row("github"),
            crate::dialogs::settings::ids::row("gitlab"),
            crate::dialogs::settings::ids::row("bitbucket"),
        ] {
            assert!(window.try_find(id.clone()).is_some(), "the group row {id}");
        }
        let (github, gitlab, bitbucket) = (
            bounds(window, crate::dialogs::settings::ids::row("github")),
            bounds(window, crate::dialogs::settings::ids::row("gitlab")),
            bounds(window, crate::dialogs::settings::ids::row("bitbucket")),
        );
        assert!(github.origin.y < gitlab.origin.y && gitlab.origin.y < bitbucket.origin.y);
        for id in [
            ids::GITHUB_TOKEN,
            ids::GITHUB_ENTERPRISE,
            ids::GITLAB_TOKEN,
            ids::GITLAB_SELF_HOSTED,
            ids::BITBUCKET_TOKEN,
        ] {
            assert!(window.try_find(id).is_some(), "the button {id}");
        }
        assert!(
            window.try_find(ids::GITHUB_BROWSER).is_none(),
            "no OAuth client id: personal access tokens only"
        );
        // The connected account sits between the GitHub and the GitLab group rows.
        let row = bounds(window, ids::row(Forge::GitHub, "octocat"));
        assert!(row.origin.y > github.origin.y && row.origin.y < gitlab.origin.y);
        let avatar = bounds(window, ids::avatar(Forge::GitHub, "octocat"));
        assert_eq!(
            (avatar.size.width.as_f32(), avatar.size.height.as_f32()),
            (AVATAR_SIZE, AVATAR_SIZE)
        );
        assert!(
            window
                .try_find(ids::check(Forge::GitHub, "octocat"))
                .is_some()
        );
        assert!(
            window
                .try_find(ids::forget(Forge::GitHub, "octocat"))
                .is_some()
        );
    })
    .expect("window");
    // The token was checked with the forge when the page loaded.
    let rows = cx.update(|cx| {
        accounts::installed(cx)
            .expect("model")
            .read(cx)
            .rows()
            .to_vec()
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].info.name, "octocat");
    assert!(
        rows[0].info.caption.starts_with("on 127.0.0.1"),
        "{}",
        rows[0].info.caption
    );
    assert_eq!(rows[0].validity, accounts::RowValidity::Valid);
    // Forget removes the account and its token; the tab's forge goes with it.
    cx.update_window(w.handle, |_, window, cx| {
        window.click(ids::forget(Forge::GitHub, "octocat"), cx);
    })
    .expect("window");
    for _ in 0..20 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    let remaining = cx.update(|cx| {
        accounts::installed(cx)
            .expect("model")
            .read(cx)
            .rows()
            .len()
    });
    assert_eq!(remaining, 0, "the account is gone");
    settle(cx, &w.controller);
    assert_eq!(
        cx.update(|cx| w.controller.read(cx).status().clone()),
        ForgeStatus::NoForge,
        "without the account the self-hosted forge is not recognised any more"
    );
}

#[gpui_kit::test]
fn sign_in_with_browser_only_with_client_id(cx: &mut TestAppContext) {
    use crate::dialogs::settings::accounts::ids;
    for (client_id, shown) in [(None, false), (Some("client-id"), true)] {
        let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
        let tmp = tempfile::tempdir().expect("a temporary directory");
        let services = services_in(tmp.path(), client_id);
        cx.update(|cx| crate::app::state_mut(cx).expect("state").services = Some(services));
        open_accounts_page(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.try_find(ids::GITHUB_BROWSER).is_some(),
                shown,
                "client id {client_id:?}"
            );
            assert!(
                window.try_find(ids::GITHUB_TOKEN).is_some(),
                "the token button is always there"
            );
        })
        .expect("window");
    }
}

#[gpui_kit::test]
fn the_page_is_missing_without_services(cx: &mut TestAppContext) {
    let (_handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    assert!(
        !cx.update(|cx| accounts::available(cx)),
        "no services, no Accounts page"
    );
    let categories = cx.update(|cx| crate::dialogs::settings::categories(cx));
    assert!(
        !categories
            .iter()
            .any(|(_, pages)| pages.contains(&crate::dialogs::settings::SettingsPage::Accounts))
    );
}

#[gpui_kit::test]
fn token_dialog_signs_in_or_explains(cx: &mut TestAppContext) {
    use super::flows::{OpenToken, TokenFlow, ids as flow_ids, open_token_dialog};
    let (handle, app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    let fake = FakeForge::start();
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), None);
    cx.update(|cx| crate::app::state_mut(cx).expect("state").services = Some(services));
    cx.update_window(handle, |_, window, cx| {
        open_token_dialog(window, cx, TokenFlow::GitHubEnterprise);
    })
    .expect("window");
    cx.run_until_parked();
    let fill = |cx: &mut TestAppContext, first: &str, token: &str| {
        cx.update_window(handle, |_, window, cx| {
            let dialog = cx
                .global::<OpenToken>()
                .0
                .upgrade()
                .expect("the dialog is open");
            dialog.update(cx, |d, cx| d.fill(Some(first), token, window, cx));
        })
        .expect("window");
        cx.run_until_parked();
    };
    // Nothing typed: the primary button does nothing.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(flow_ids::TOKEN_FIRST).is_some()
                && window.try_find(flow_ids::TOKEN_FIELD).is_some()
        );
        assert!(window.try_find(flow_ids::TOKEN_HELP).is_some());
        window.click("dialog-primary", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| accounts::installed(cx)
            .expect("model")
            .read(cx)
            .rows()
            .len()),
        0
    );
    // A rejected token keeps the dialog open with the reason; nothing is stored.
    fake.server.set_unauthorized(true);
    fill(cx, &fake.server.api_url(), "bad-token");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    for _ in 0..10 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    let error = cx.update(|cx| {
        cx.global::<OpenToken>()
            .0
            .upgrade()
            .expect("still open")
            .read(cx)
            .error()
            .map(str::to_owned)
    });
    assert!(
        error.is_some_and(|e| e.contains("rejected")),
        "the reason is shown"
    );
    // The right token signs in: the dialog closes, the account appears, a toast says so.
    fake.server.set_unauthorized(false);
    fill(cx, &fake.server.api_url(), "good-token");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    for _ in 0..20 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    assert!(
        !dialog_open(cx, handle),
        "the dialog closed after the sign-in"
    );
    let rows = cx.update(|cx| {
        accounts::installed(cx)
            .expect("model")
            .read(cx)
            .rows()
            .to_vec()
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].info.name, "octocat");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t == "Signed in to GitHub as octocat"),
        "{toasts:?}"
    );
}

fn device_account(name: &str) -> crate::backend::forge::AccountInfo {
    crate::backend::forge::AccountInfo {
        forge: crate::backend::forge::Forge::GitHub,
        name: name.into(),
        caption: "browser sign-in".into(),
        avatar_url: None,
        id: crate::backend::forge::AccountId("{}".into()),
    }
}

fn device_code(user_code: &str, expires_in: u64) -> DeviceCodeInfo {
    DeviceCodeInfo {
        user_code: user_code.into(),
        device_code: "device".into(),
        verification_uri: "https://github.com/login/device".into(),
        interval: 1,
        expires_in,
    }
}

#[gpui_kit::test]
fn device_flow_dialog_states(cx: &mut TestAppContext) {
    use std::sync::Mutex;

    use super::flows::{
        DeviceFlowBackend, DeviceState, OpenDevice, ids as flow_ids, open_device_dialog,
    };
    let (handle, app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), Some("client-id"));
    cx.update(|cx| crate::app::state_mut(cx).expect("state").services = Some(services));
    let polls = Arc::new(Mutex::new(0u32));
    let counter = polls.clone();
    cx.update(|cx| {
        cx.set_global(DeviceFlowBackend {
            start: Arc::new(|| Ok(device_code("ABCD-1234", 900))),
            poll: Arc::new(move |_, _| {
                let mut n = counter.lock().expect("counter");
                *n += 1;
                if *n < 3 {
                    Ok(DevicePoll::Pending)
                } else {
                    Ok(DevicePoll::SignedIn(device_account("alice")))
                }
            }),
        });
    });
    let opened = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = opened.clone();
    let copied = Rc::new(RefCell::new(Vec::<String>::new()));
    let copy_sink = copied.clone();
    cx.update(|cx| {
        cx.set_global(DialogHooks {
            open_url: Some(Rc::new(move |url| sink.borrow_mut().push(url.to_owned()))),
            copy: Some(Rc::new(move |text| {
                copy_sink.borrow_mut().push(text.to_owned())
            })),
            ..DialogHooks::default()
        });
    });
    cx.update_window(handle, |_, window, cx| open_device_dialog(window, cx))
        .expect("window");
    cx.run_until_parked();
    let state = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            cx.global::<OpenDevice>()
                .0
                .upgrade()
                .map(|d| d.read(cx).state().clone())
        })
    };
    assert!(
        matches!(state(cx), Some(DeviceState::Waiting(ref code)) if code.user_code == "ABCD-1234")
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(flow_ids::DEVICE_CODE).is_some(),
            "the code is shown"
        );
        assert!(
            window.try_find(flow_ids::DEVICE_PROGRESS).is_some(),
            "a spinner while waiting"
        );
        window.click(flow_ids::DEVICE_COPY, cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    assert_eq!(
        copied.borrow().as_slice(),
        ["ABCD-1234"],
        "Copy copies the code"
    );
    assert_eq!(
        opened.borrow().as_slice(),
        ["https://github.com/login/device"],
        "Open browser opens the verification page"
    );
    // Two polls answer pending, the third signs in: the dialog closes and a toast says so.
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    }
    assert_eq!(*polls.lock().expect("counter"), 3);
    assert!(!dialog_open(cx, handle), "signed in: the dialog is closed");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t == "Signed in to GitHub as alice"),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn device_flow_dialog_expires_and_fails(cx: &mut TestAppContext) {
    use super::flows::{
        DeviceFlowBackend, DeviceState, OpenDevice, ids as flow_ids, open_device_dialog,
    };
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let services = services_in(tmp.path(), Some("client-id"));
    cx.update(|cx| crate::app::state_mut(cx).expect("state").services = Some(services));
    cx.update(|cx| {
        cx.set_global(DeviceFlowBackend {
            start: Arc::new(|| Ok(device_code("WXYZ-0000", 3))),
            poll: Arc::new(|_, _| Ok(DevicePoll::Pending)),
        });
    });
    cx.update_window(handle, |_, window, cx| open_device_dialog(window, cx))
        .expect("window");
    cx.run_until_parked();
    for _ in 0..4 {
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    }
    let state = cx.update(|cx| {
        cx.global::<OpenDevice>()
            .0
            .upgrade()
            .map(|d| d.read(cx).state().clone())
    });
    assert!(
        matches!(state, Some(DeviceState::Failed(ref m)) if m.contains("expired")),
        "the code expired: {state:?}"
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(flow_ids::DEVICE_PROGRESS).is_none(),
            "no spinner after the end"
        );
        assert!(
            window.try_find(flow_ids::DEVICE_STATUS).is_some(),
            "the reason is shown"
        );
    })
    .expect("window");
    // A start that fails shows the reason too.
    cx.update(|cx| {
        cx.set_global(DeviceFlowBackend {
            start: Arc::new(|| Err(failure(FailureKind::Offline))),
            poll: Arc::new(|_, _| Ok(DevicePoll::Pending)),
        });
    });
    cx.update_window(handle, |_, window, cx| {
        use gpui_kit::component::WindowExt as _;
        window.close_dialog(cx);
        open_device_dialog(window, cx);
    })
    .expect("window");
    cx.run_until_parked();
    let state = cx.update(|cx| {
        cx.global::<OpenDevice>()
            .0
            .upgrade()
            .map(|d| d.read(cx).state().clone())
    });
    assert!(matches!(state, Some(DeviceState::Failed(_))), "{state:?}");
}

// ---- HTTPS prompts ---------------------------------------------------------------------------------------------

fn request(kind: crate::backend::PromptKind) -> crate::backend::CredentialRequest {
    crate::backend::CredentialRequest {
        kind,
        raw_prompt: String::new(),
        operation: "Push to origin".into(),
        repo: None,
        attempt: 1,
        cancel: crate::backend::CancelToken::new(),
    }
}

#[gpui_kit::test]
fn https_prompt_offers_account(cx: &mut TestAppContext) {
    use crate::backend::PromptKind;
    use crate::dialogs::credentials::{self, ids};
    let w = world(cx, &[], true);
    let url = w.fake.server.remote_url();
    let password = request(PromptKind::HttpsPassword {
        url: url.clone(),
        username: None,
    });
    let offer = cx
        .update(|cx| accounts::https_offer(&password, cx))
        .expect("the account of the host is offered");
    assert_eq!(offer.label(), "Use GitHub account octocat");
    assert_eq!(offer.username(), "octocat");
    // Another host, another user name, other prompt kinds, or the setting off: no offer.
    let other_host = request(PromptKind::HttpsPassword {
        url: "https://example.org/x.git".into(),
        username: None,
    });
    assert!(
        cx.update(|cx| accounts::https_offer(&other_host, cx))
            .is_none()
    );
    let other_user = request(PromptKind::HttpsPassword {
        url: url.clone(),
        username: Some("mallory".into()),
    });
    assert!(
        cx.update(|cx| accounts::https_offer(&other_user, cx))
            .is_none()
    );
    let same_user = request(PromptKind::HttpsPassword {
        url: url.clone(),
        username: Some("Octocat".into()),
    });
    assert!(
        cx.update(|cx| accounts::https_offer(&same_user, cx))
            .is_some()
    );
    let ssh = request(PromptKind::SshPassphrase { key_path: None });
    assert!(cx.update(|cx| accounts::https_offer(&ssh, cx)).is_none());
    cx.update(|cx| crate::settings::update(cx, |s| s.forge_https_answers = false));
    assert!(
        cx.update(|cx| accounts::https_offer(&password, cx))
            .is_none(),
        "the setting turns the offer off"
    );
    cx.update(|cx| crate::settings::update(cx, |s| s.forge_https_answers = true));

    // The password prompt: the checkbox answers with the token (read from the keychain off the UI thread).
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    cx.update_window(w.handle, |_, window, cx| {
        credentials::open(window, cx, password.clone(), tx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(ids::CREDENTIAL_USE_ACCOUNT).is_some(),
            "the offer is a checkbox"
        );
        window.click(ids::CREDENTIAL_USE_ACCOUNT, cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    for _ in 0..5 {
        cx.run_until_parked();
    }
    match rx.try_recv() {
        Ok(crate::backend::CredentialResponse::Answer(token)) => {
            assert_eq!(
                token.expose(),
                "secret-token",
                "the stored token answers the password prompt"
            );
        }
        _ => panic!("the dialog should have answered with the account's token"),
    }
    // A password prompt whose URL names the account's login starts with the box checked; one without a user
    // name does not. Plain Enter-through is enough to answer with the token.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let named = request(PromptKind::HttpsPassword {
        url: url.clone(),
        username: Some("octocat".into()),
    });
    cx.update_window(w.handle, |_, window, cx| {
        credentials::open(window, cx, named, tx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(ids::CREDENTIAL_USE_ACCOUNT).is_some(),
            "the offer is a checkbox"
        );
        window.click("dialog-primary", cx);
    })
    .expect("window");
    for _ in 0..5 {
        cx.run_until_parked();
    }
    match rx.try_recv() {
        Ok(crate::backend::CredentialResponse::Answer(token)) => {
            assert_eq!(
                token.expose(),
                "secret-token",
                "pre-checked: the token answers"
            );
        }
        _ => panic!("the pre-checked box should answer with the account's token"),
    }
    // The user name prompt: the account's login.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let username = request(PromptKind::HttpsUsername { url });
    cx.update_window(w.handle, |_, window, cx| {
        credentials::open(window, cx, username, tx)
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(w.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(ids::CREDENTIAL_USE_ACCOUNT, cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    cx.run_until_parked();
    match rx.try_recv() {
        Ok(crate::backend::CredentialResponse::Answer(name)) => {
            assert_eq!(name.expose(), "octocat");
        }
        _ => panic!("the dialog should have answered with the login"),
    }
}

// ---- pull request checkout -------------------------------------------------------------------------------------

fn bare_with(fixture: &Fixture, refs: &[&str]) -> String {
    let bare = fixture.dir.path().join("fork.git");
    let text = bare.to_string_lossy().into_owned();
    fixture.git(&["init", "-q", "--bare", &text]);
    for name in refs {
        fixture.git(&["push", "-q", &text, &format!("HEAD:{name}")]);
    }
    text
}

fn run_checkout(cx: &mut TestAppContext, w: &World, plan: crate::backend::forge::CheckoutPlanInfo) {
    cx.update_window(w.handle, |_, window, cx| {
        w.tab.update(cx, |tab, cx| {
            tab.check_out_pull_request(plan, 7, window, cx);
        });
    })
    .expect("window");
    for _ in 0..30 {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(50));
    }
}

#[gpui_kit::test]
fn checkout_of_a_fork_pull_request_adds_the_remote_and_tracks_its_branch(cx: &mut TestAppContext) {
    let w = world(cx, &[], true);
    let bare = bare_with(&w.fixture, &["refs/heads/pr-branch"]);
    run_checkout(
        cx,
        &w,
        crate::backend::forge::CheckoutPlanInfo {
            remote_name: "octo".into(),
            remote_url: bare,
            fetch_refspec: "+refs/heads/pr-branch:refs/remotes/octo/pr-branch".into(),
            local_branch: "octo-pr-branch".into(),
            fetched_ref: "refs/remotes/octo/pr-branch".into(),
            upstream: Some("refs/remotes/octo/pr-branch".into()),
        },
    );
    assert_eq!(
        w.fixture.git(&["branch", "--show-current"]).trim(),
        "octo-pr-branch",
        "checked out"
    );
    assert!(
        w.fixture.git(&["remote"]).lines().any(|r| r == "octo"),
        "the remote was added"
    );
    assert_eq!(
        w.fixture
            .git(&["config", "branch.octo-pr-branch.remote"])
            .trim(),
        "octo"
    );
    assert_eq!(
        w.fixture
            .git(&["config", "branch.octo-pr-branch.merge"])
            .trim(),
        "refs/heads/pr-branch",
        "the fork's branch is the upstream"
    );
    let toasts = cx.update(|cx| toast_texts(&w.app, cx));
    assert!(
        toasts
            .iter()
            .any(|t| t == "Checked out pull request #7 as 'octo-pr-branch'"),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn checkout_of_a_pull_request_ref_sets_no_upstream(cx: &mut TestAppContext) {
    let w = world(cx, &[], true);
    let bare = bare_with(&w.fixture, &["refs/pull/7/head"]);
    run_checkout(
        cx,
        &w,
        crate::backend::forge::CheckoutPlanInfo {
            remote_name: "octo".into(),
            remote_url: bare,
            fetch_refspec: "+refs/pull/7/head:refs/remotes/octo/pull/7".into(),
            local_branch: "pr-7".into(),
            fetched_ref: "refs/remotes/octo/pull/7".into(),
            upstream: None,
        },
    );
    assert_eq!(w.fixture.git(&["branch", "--show-current"]).trim(), "pr-7");
    let has_upstream = w
        .fixture
        .git(&["for-each-ref", "--format=%(upstream)", "refs/heads/pr-7"]);
    assert!(
        has_upstream.trim().is_empty(),
        "no --track and no upstream for the pull request's own ref"
    );
}

#[gpui_kit::test]
fn checkout_asks_for_another_name_when_the_branch_exists(cx: &mut TestAppContext) {
    let w = world(cx, &[], true);
    let bare = bare_with(&w.fixture, &["refs/heads/pr-branch"]);
    run_checkout(
        cx,
        &w,
        crate::backend::forge::CheckoutPlanInfo {
            remote_name: "octo".into(),
            remote_url: bare,
            fetch_refspec: "+refs/heads/pr-branch:refs/remotes/octo/pr-branch".into(),
            local_branch: "newfeat".into(),
            fetched_ref: "refs/remotes/octo/pr-branch".into(),
            upstream: Some("refs/remotes/octo/pr-branch".into()),
        },
    );
    assert!(
        dialog_open(cx, w.handle),
        "a local branch of that name exists: the user picks another name"
    );
    assert_eq!(
        w.fixture.git(&["branch", "--show-current"]).trim(),
        "main",
        "nothing was switched"
    );
}
