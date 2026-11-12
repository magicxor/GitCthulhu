use core::prelude::v1::test;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, IntoElement as _, Modifiers, MouseButton,
    Styled as _, TestAppContext, Window,
};

use super::bottom_bar::{self, EMAIL_NOT_SET, NAME_NOT_SET};
use super::platform::{self, OnPicked, ShellPlatform};
use super::recent_repos::{self, RecentRepos, RecentReposEvent};
use super::{dims, ids};
use crate::app_view::{AppView, TabContent};
use crate::backend::handle::TabBackend;
use crate::backend::{GitStatus, Identity, IdentityInfo, PartialIdentity};
use crate::metrics::{self, dp};
use crate::settings::state;
use crate::tab::ops::{Cancellable, OpKind, OpSpec};
use crate::tab::{BusyState, PrompterMode, RepoTab, TabId, TabOptions};
use crate::test_support::geometry::{assert_rect, bounds, open_probe};
use crate::test_support::input::click_with;
use crate::test_support::{
    DEFAULT_SIZE, Fixture, InertBackend, RepoSeed, init_app, mount_seeded, open_app, toast_texts,
};
use crate::theme::ActiveTokens as _;
use crate::widgets::observed;

/// What the fake platform saw.
#[derive(Clone, Default)]
struct Recorder {
    urls: Rc<RefCell<Vec<String>>>,
    picks: Rc<Cell<usize>>,
    answer: Rc<RefCell<Option<PathBuf>>>,
}

impl Recorder {
    fn install(&self, cx: &mut TestAppContext) {
        let (urls, picks, answer) = (self.urls.clone(), self.picks.clone(), self.answer.clone());
        cx.update(|cx| {
            platform::install(
                cx,
                ShellPlatform::new(
                    move |window: &mut Window, cx: &mut App, done: OnPicked| {
                        picks.set(picks.get() + 1);
                        let choice = answer.borrow().clone();
                        if let Some(path) = choice {
                            done(path, window, cx);
                        }
                    },
                    move |_, url| urls.borrow_mut().push(url.to_owned()),
                ),
            )
        });
    }
}

fn frame<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx)
    })
    .expect("window")
}

fn add_inert_tab(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    app: &Entity<AppView>,
    name: &str,
) -> Entity<RepoTab> {
    cx.update_window(handle, |_, window, cx| {
        let tab = cx.new(|cx| {
            RepoTab::new(
                TabId::next(),
                TabBackend::Inert(std::sync::Arc::new(InertBackend::default())),
                name.to_owned().into(),
                PathBuf::from(name),
                TabOptions {
                    watch: false,
                    prompter: PrompterMode::Refuse,
                },
                window,
                cx,
            )
        });
        app.update(cx, |app, cx| {
            app.add_repo_tab(tab.clone(), true, window, cx)
        });
        tab
    })
    .expect("window")
}

/// An app whose tabs are the inert repositories `names` (the last one selected).
fn app_with_tabs(cx: &mut TestAppContext, names: &[&str]) -> (AnyWindowHandle, Entity<AppView>) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    for name in names {
        add_inert_tab(cx, handle, &app, name);
    }
    cx.run_until_parked();
    (handle, app)
}

fn titles(app: &Entity<AppView>, cx: &App) -> Vec<String> {
    let app = app.read(cx);
    (0..app.tabs().len())
        .map(|ix| app.tab_title(ix, cx).to_string())
        .collect()
}

fn active(app: &Entity<AppView>, cx: &App) -> usize {
    app.read(cx).active()
}

fn git_ready(cx: &mut TestAppContext, fixture: &Fixture) {
    let rt = fixture.runtime();
    cx.update(|cx| {
        if let Some(state) = crate::app::state_mut(cx) {
            state.git = GitStatus::Ready(rt);
        }
    });
}

fn set_recent(cx: &mut TestAppContext, paths: Vec<PathBuf>) {
    cx.update(|cx| state::update(cx, |s| s.recent_repositories = paths));
}

/// Whether a dialog is open (the real one: a "Not implemented yet" toast no longer counts).
fn dialog_is_open(app: &Entity<AppView>, window: &mut Window, cx: &mut App) -> bool {
    window.has_active_dialog(cx)
        && !toast_texts(app, cx)
            .iter()
            .any(|t| t.starts_with("Not implemented yet"))
}

// ---------------------------------------------------------------------------------------------------------------
// tab strip (07 §2)
// ---------------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn tab_strip_geometry(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta", "gamma"]);
    assert_eq!(cx.update(|cx| titles(&app, cx)), ["alpha", "beta", "gamma"]);
    frame(cx, handle, |window, _| {
        assert_rect(
            window,
            crate::ids::TAB_STRIP,
            0.,
            0.,
            1280.,
            metrics::TAB_STRIP_H,
        );
        let mut right = 0.;
        for ix in 0..3usize {
            let b = bounds(window, (ids::TAB, ix));
            assert!(
                (b.size.height.as_f32() - 36.).abs() <= 0.5,
                "tab {ix} is 36 high"
            );
            assert!(
                b.size.width.as_f32() >= 200. - 0.5,
                "tab {ix} is at least 200 wide"
            );
            assert!((b.origin.y.as_f32()).abs() <= 0.5);
            assert!(
                (b.origin.x.as_f32() - right).abs() <= 0.5,
                "tab {ix} follows the previous one"
            );
            right = (b.origin.x + b.size.width).as_f32();
        }
        let selected = bounds(window, (ids::TAB, 2usize));
        assert!(
            (selected.size.width.as_f32() - 200.).abs() <= 0.5,
            "a short title gives the 200 dp minimum width"
        );
        // The close button reserves 8 + 14 + 8 at the tab's right edge.
        let close = bounds(window, (ids::TAB_CLOSE, 2usize));
        assert!((close.size.width.as_f32() - 30.).abs() <= 0.5);
        assert!((close.right().as_f32() - selected.right().as_f32()).abs() <= 0.5);
        assert!((close.center().y.as_f32() - 18.).abs() <= 0.5);
        let underline = bounds(window, ids::TAB_UNDERLINE);
        assert!((underline.size.height.as_f32() - 3.).abs() <= 0.5);
        assert!((underline.size.width.as_f32() - selected.size.width.as_f32()).abs() <= 0.5);
        assert!((underline.origin.x.as_f32() - selected.origin.x.as_f32()).abs() <= 0.5);
        assert!(
            (underline.origin.y.as_f32() - 33.).abs() <= 0.5,
            "the underline sits at the bottom edge of the tab"
        );
        assert_rect(window, ids::TAB_ADD, right, 0., 36., 36.);
        assert!(
            window.try_find(ids::TAB_SCROLLBAR).is_none(),
            "no scrollbar while the tabs fit"
        );
    });
}

#[gpui_kit::test]
fn a_long_title_caps_the_tab_at_766(cx: &mut TestAppContext) {
    // 16 + 720 (text cap) + 30 (close) = 766 (07 §2.2).
    let long = "a-very-long-repository-name-".repeat(20);
    let (handle, _app) = app_with_tabs(cx, &[long.as_str(), "short"]);
    frame(cx, handle, |window, _| {
        let long_tab = bounds(window, (ids::TAB, 0usize));
        assert!(
            (long_tab.size.width.as_f32() - 766.).abs() <= 0.5,
            "the long tab is 766 wide, got {}",
            long_tab.size.width.as_f32()
        );
        let short = bounds(window, (ids::TAB, 1usize));
        assert!((short.origin.x.as_f32() - 766.).abs() <= 0.5);
    });
}

#[gpui_kit::test]
fn window_content_is_tab_strip_plus_frame(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        assert_rect(window, crate::ids::TAB_STRIP, 0., 0., 1280., 36.);
        assert_rect(window, crate::ids::TAB_AREA, 0., 36., 1280., 720. - 36.);
        assert_rect(window, crate::ids::BOTTOM_BAR, 0., 720. - 32., 1280., 32.);
        assert_rect(window, crate::ids::FRAME_DIVIDER, 0., 720. - 33., 1280., 1.);
    });
}

#[gpui_kit::test]
fn clicking_a_tab_selects_it_and_the_plus_appends_an_empty_tab(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta"]);
    assert_eq!(cx.update(|cx| active(&app, cx)), 1);
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB, 0usize), cx)
    });
    assert_eq!(
        cx.update(|cx| active(&app, cx)),
        0,
        "the click selected the first tab"
    );
    frame(cx, handle, |window, cx| window.click(ids::TAB_ADD, cx));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(titles(&app, cx), ["alpha", "beta", "New tab"]);
        assert_eq!(active(&app, cx), 2, "the new empty tab is selected");
        assert!(matches!(
            app.read(cx).tabs()[2].content,
            TabContent::Welcome(_)
        ));
    });
}

#[gpui_kit::test]
fn tabs_restore_and_select(cx: &mut TestAppContext) {
    let (one, two) = (Fixture::new(), Fixture::new());
    for (fx, text) in [(&one, "1\n"), (&two, "2\n")] {
        fx.write("a.txt", text);
        fx.commit_all("init");
    }
    init_app(cx);
    git_ready(cx, &one);
    cx.update(|cx| {
        state::update(cx, |s| {
            s.tabs_open = vec![one.path().to_owned(), two.path().to_owned()];
            s.selected_tab = 1;
        })
    });
    let (_handle, app) = open_app(cx, DEFAULT_SIZE);
    cx.run_until_parked();
    cx.update(|cx| {
        let view = app.read(cx);
        assert_eq!(view.tabs().len(), 2, "both persisted tabs were restored");
        assert!(
            view.tabs().iter().all(|s| s.repo().is_some()),
            "both opened"
        );
        assert_eq!(view.active(), 1, "the persisted selection was restored");
        assert_eq!(view.tabs()[0].path(cx).as_deref(), Some(one.path()));
        assert_eq!(view.tabs()[1].path(cx).as_deref(), Some(two.path()));
    });
}

#[gpui_kit::test]
fn close_tab_selects_left(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta", "gamma", "delta"]);
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB, 2usize), cx)
    });
    // The close button of the selected tab.
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB_CLOSE, 2usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(titles(&app, cx), ["alpha", "beta", "delta"]);
        assert_eq!(active(&app, cx), 1, "the tab to the left is selected");
    });
    // Closing the first tab while it is selected selects its right neighbour, now at index 0.
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB, 0usize), cx)
    });
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB_CLOSE, 0usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(titles(&app, cx), ["beta", "delta"]);
        assert_eq!(active(&app, cx), 0);
    });
}

#[gpui_kit::test]
fn closing_an_unselected_tab_keeps_the_selection(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta", "gamma"]);
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB_CLOSE, 0usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(titles(&app, cx), ["beta", "gamma"]);
        assert_eq!(active(&app, cx), 1, "gamma stays selected");
    });
}

#[gpui_kit::test]
fn last_tab_replaced_by_empty(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha"]);
    frame(cx, handle, |window, cx| {
        window.click((ids::TAB_CLOSE, 0usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            titles(&app, cx),
            ["New tab"],
            "the window never has zero tabs"
        );
        assert!(matches!(
            app.read(cx).tabs()[0].content,
            TabContent::Welcome(_)
        ));
    });
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::WELCOME).is_some());
    });
}

#[gpui_kit::test]
fn middle_click_closes(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta", "gamma"]);
    cx.update_window(handle, |_, window, cx| {
        click_with(
            window,
            cx,
            (ids::TAB, 1usize),
            Modifiers::default(),
            MouseButton::Middle,
        )
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            titles(&app, cx),
            ["alpha", "gamma"],
            "the middle click closed beta"
        );
        assert_eq!(
            active(&app, cx),
            1,
            "gamma, the selected tab, stays selected"
        );
    });
}

#[gpui_kit::test]
fn right_click_on_a_tab_does_nothing(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta"]);
    frame(cx, handle, |window, cx| {
        window.right_click((ids::TAB, 0usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(titles(&app, cx), ["alpha", "beta"]);
        assert_eq!(active(&app, cx), 1);
    });
}

#[gpui_kit::test]
fn drag_reorders_and_persists(cx: &mut TestAppContext) {
    let (handle, app) = app_with_tabs(cx, &["alpha", "beta", "gamma"]);
    let (from, to) = frame(cx, handle, |window, _| {
        let first = bounds(window, (ids::TAB, 0usize));
        let last = bounds(window, (ids::TAB, 2usize));
        (
            first.center(),
            gpui_kit::point(last.right() - gpui_kit::px(20.), last.center().y),
        )
    });
    cx.update_window(handle, |_, window, cx| window.drag(from, to, cx))
        .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            titles(&app, cx),
            ["beta", "gamma", "alpha"],
            "the dragged tab moved to where it was dropped"
        );
        assert_eq!(
            active(&app, cx),
            2,
            "the dragged tab was selected and stays so"
        );
        let persisted: Vec<String> = state::get(cx)
            .tabs_open
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            persisted,
            ["beta", "gamma", "alpha"],
            "the order was persisted"
        );
        assert_eq!(state::get(cx).selected_tab, 2);
    });
    // And back to the front.
    let (from, to) = frame(cx, handle, |window, _| {
        let last = bounds(window, (ids::TAB, 2usize));
        let first = bounds(window, (ids::TAB, 0usize));
        (
            last.center(),
            gpui_kit::point(first.left() + gpui_kit::px(10.), first.center().y),
        )
    });
    cx.update_window(handle, |_, window, cx| window.drag(from, to, cx))
        .expect("window");
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(titles(&app, cx), ["alpha", "beta", "gamma"]));
}

#[test]
fn drag_target_counts_midpoints_left_of_the_pointer() {
    use super::tab_strip::drag_target_index;
    let mids = [100., 300., 500.];
    assert_eq!(drag_target_index(&mids, 0, 50.), 0);
    assert_eq!(drag_target_index(&mids, 0, 350.), 1, "past beta's midpoint");
    assert_eq!(drag_target_index(&mids, 0, 900.), 2);
    assert_eq!(drag_target_index(&mids, 2, 50.), 0);
    assert_eq!(
        drag_target_index(&mids, 1, 301.),
        1,
        "stays between the neighbours"
    );
}

#[test]
fn autoscroll_scrolls_by_the_overshoot_and_clamps() {
    use super::tab_strip::autoscroll_offset;
    let view = (0., 1000.);
    assert_eq!(
        autoscroll_offset(-100., 500., view, 24., 500.),
        -100.,
        "middle: nothing"
    );
    assert_eq!(
        autoscroll_offset(-100., 500., view, 24., 14.),
        -90.,
        "10 into the left edge zone"
    );
    assert_eq!(
        autoscroll_offset(-100., 500., view, 24., 1010.),
        -134.,
        "34 into the right edge zone"
    );
    assert_eq!(
        autoscroll_offset(-5., 500., view, 24., 0.),
        0.,
        "never past the start"
    );
    assert_eq!(
        autoscroll_offset(-490., 500., view, 24., 1100.),
        -500.,
        "never past the end"
    );
}

#[gpui_kit::test]
fn overflowing_tabs_show_the_scrollbar_and_pin_the_plus(cx: &mut TestAppContext) {
    let names: Vec<String> = (0..9).map(|n| format!("repository-{n}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let (handle, _app) = app_with_tabs(cx, &names);
    // The first frame measures the overflow, the second shows the bar.
    frame(cx, handle, |_, _| ());
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::TAB_SCROLLBAR).is_some(),
            "9 x 200 > 1280"
        );
        assert_rect(
            window,
            crate::ids::TAB_STRIP,
            0.,
            0.,
            1280.,
            metrics::TAB_SCROLLBAR_H + metrics::TAB_STRIP_H,
        );
        assert_rect(
            window,
            ids::TAB_ADD,
            1280. - 36.,
            metrics::TAB_SCROLLBAR_H,
            36.,
            36.,
        );
    });
}

#[gpui_kit::test]
fn opening_slot_shows_loading_screen_and_path_title(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    // Git never becomes ready in this test: the slot stays in "Opening".
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.open_repository(PathBuf::from("/nowhere/myrepo.git"), true, window, cx)
        })
    })
    .expect("window");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            titles(&app, cx),
            ["myrepo"],
            "the title of an opening tab is the directory name"
        );
        assert_eq!(
            app.read(cx).tab_tooltip(0, cx).as_deref(),
            Some(
                Path::new("/nowhere/myrepo.git")
                    .display()
                    .to_string()
                    .as_str()
            ),
            "the tooltip is the full path"
        );
    });
    frame(cx, handle, |window, _| {
        assert_rect(window, ids::LOADING, 0., 36., 1280., 720. - 36.);
        assert!(window.try_find(ids::TAB_UNDERLINE).is_some());
    });
}

// ---------------------------------------------------------------------------------------------------------------
// welcome page (07 §4)
// ---------------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn welcome_geometry(cx: &mut TestAppContext) {
    init_app(cx);
    set_recent(cx, vec![PathBuf::from("/work/alpha")]);
    let (handle, _app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let group = bounds(window, ids::WELCOME_GROUP);
        assert!(
            (group.size.width.as_f32() - 928.).abs() <= 0.5,
            "the group is 928 wide"
        );
        assert!(
            (group.origin.x.as_f32() - (1280. - 928.) / 2.).abs() <= 0.5,
            "the group is centred"
        );
        // The free height above the group is 25 % of the free height around it (bias -0.5).
        let area_h = 720. - 36. - 1. - 32.;
        let top = group.origin.y.as_f32() - 36.;
        let expected = 0.25 * (area_h - group.size.height.as_f32());
        assert!(
            (top - expected).abs() <= 0.5,
            "top {top}, expected {expected}"
        );
        for id in [ids::WELCOME_OPEN, ids::WELCOME_CLONE, ids::WELCOME_INIT] {
            let b = bounds(window, id);
            assert!(
                (b.size.width.as_f32() - 280.).abs() <= 0.5,
                "{id} is 280 wide"
            );
            assert!(
                (b.size.height.as_f32() - 56.).abs() <= 0.5,
                "{id} is 56 high"
            );
            assert!((b.origin.x.as_f32() - group.origin.x.as_f32()).abs() <= 0.5);
        }
        for id in [
            ids::WELCOME_SOURCE,
            ids::WELCOME_BUG,
            ids::WELCOME_ABOUT,
            ids::WELCOME_SETTINGS,
        ] {
            let b = bounds(window, id);
            assert!(
                (b.size.width.as_f32() - 280.).abs() <= 0.5,
                "{id} is 280 wide"
            );
            assert!(
                (b.size.height.as_f32() - 40.).abs() <= 0.5,
                "{id} is 40 high"
            );
        }
        // Tiles are 56 + 8 apart.
        let (open, clone) = (
            bounds(window, ids::WELCOME_OPEN),
            bounds(window, ids::WELCOME_CLONE),
        );
        assert!((clone.origin.y.as_f32() - open.origin.y.as_f32() - 64.).abs() <= 0.5);
        // The recent list is 600 x 400, 16 + 32 right of the buttons.
        let recent = bounds(window, ids::RECENT_REPOS);
        assert!((recent.size.width.as_f32() - 600.).abs() <= 0.5);
        assert!((recent.size.height.as_f32() - 400.).abs() <= 0.5);
        assert!(
            (recent.origin.x.as_f32() - (group.origin.x.as_f32() + 280. + 16. + 32.)).abs() <= 0.5
        );
        // The title sits above the buttons at the group's left edge.
        let title = bounds(window, ids::WELCOME_TITLE);
        assert!((title.origin.x.as_f32() - group.origin.x.as_f32()).abs() <= 0.5);
        assert!(title.bottom().as_f32() <= open.origin.y.as_f32());
        // Divider and bottom bar.
        assert_rect(window, ids::WELCOME_DIVIDER, 0., 720. - 33., 1280., 1.);
        assert_rect(window, crate::ids::BOTTOM_BAR, 0., 720. - 32., 1280., 32.);
        assert!(
            window.try_find(ids::BOTTOM_IDENTITY).is_none(),
            "no identity on the welcome page"
        );
    });
}

#[gpui_kit::test]
fn welcome_without_recent_repositories_says_so(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, _app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::RECENT_EMPTY).is_some());
        assert!(
            window.try_find("recent-filter-field").is_none(),
            "no filter field without entries"
        );
        let recent = bounds(window, ids::RECENT_REPOS);
        assert!((recent.size.width.as_f32() - 600.).abs() <= 0.5);
        assert!((recent.size.height.as_f32() - 400.).abs() <= 0.5);
    });
}

#[gpui_kit::test]
fn welcome_links_open_the_project_pages(cx: &mut TestAppContext) {
    init_app(cx);
    let recorder = Recorder::default();
    recorder.install(cx);
    let (handle, _app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        window.click(ids::WELCOME_SOURCE, cx)
    });
    frame(cx, handle, |window, cx| window.click(ids::WELCOME_BUG, cx));
    assert_eq!(
        *recorder.urls.borrow(),
        [
            super::REPOSITORY_URL.to_owned(),
            format!("{}/issues", super::REPOSITORY_URL)
        ]
    );
}

fn welcome_button_opens(cx: &mut TestAppContext, id: &'static str, what: &str) {
    init_app(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(id, cx));
    cx.run_until_parked();
    frame(cx, handle, |window, cx| {
        assert!(
            dialog_is_open(&app, window, cx),
            "{id} opens the {what} dialog"
        );
    });
}

#[gpui_kit::test]
fn welcome_clone_opens_the_clone_dialog(cx: &mut TestAppContext) {
    welcome_button_opens(cx, ids::WELCOME_CLONE, "Clone");
}

#[gpui_kit::test]
fn welcome_info_opens_the_about_dialog(cx: &mut TestAppContext) {
    welcome_button_opens(cx, ids::WELCOME_ABOUT, "About");
}

#[gpui_kit::test]
fn welcome_settings_opens_the_settings_dialog(cx: &mut TestAppContext) {
    welcome_button_opens(cx, ids::WELCOME_SETTINGS, "Settings");
}

#[gpui_kit::test]
fn welcome_open_picks_a_directory_and_opens_it_in_this_tab(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    init_app(cx);
    git_ready(cx, &fx);
    let recorder = Recorder::default();
    recorder.install(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);

    // A cancelled picker does nothing.
    frame(cx, handle, |window, cx| window.click(ids::WELCOME_OPEN, cx));
    cx.run_until_parked();
    assert_eq!(recorder.picks.get(), 1);
    cx.update(|cx| assert_eq!(titles(&app, cx), ["New tab"]));

    *recorder.answer.borrow_mut() = Some(fx.path().to_owned());
    frame(cx, handle, |window, cx| window.click(ids::WELCOME_OPEN, cx));
    cx.run_until_parked();
    cx.update(|cx| {
        let view = app.read(cx);
        assert_eq!(
            view.tabs().len(),
            1,
            "the welcome tab was replaced, not joined"
        );
        assert!(view.active_tab().is_some(), "the repository is open");
        assert_eq!(
            state::get(cx)
                .recent_repositories
                .first()
                .map(|p| p.file_name()),
            Some(fx.path().file_name())
        );
        assert_eq!(state::get(cx).tabs_open.len(), 1, "and persisted");
    });
}

#[gpui_kit::test]
fn welcome_start_local_repository_inits_the_directory(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let fresh = fx.dir.path().join("fresh");
    std::fs::create_dir_all(&fresh).expect("directory");
    init_app(cx);
    git_ready(cx, &fx);
    let recorder = Recorder::default();
    *recorder.answer.borrow_mut() = Some(fresh.clone());
    recorder.install(cx);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(ids::WELCOME_INIT, cx));
    cx.run_until_parked();
    assert!(fresh.join(".git").exists(), "git init ran");
    cx.update(|cx| {
        assert!(
            app.read(cx).active_tab().is_some(),
            "and the repository opened in the tab"
        )
    });
}

#[gpui_kit::test]
fn welcome_recent_row_click_opens_the_repository(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    init_app(cx);
    git_ready(cx, &fx);
    set_recent(cx, vec![fx.path().to_owned()]);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        window.click((ids::RECENT_ROW, 0usize), cx)
    });
    cx.run_until_parked();
    cx.update(|cx| assert!(app.read(cx).active_tab().is_some()));
}

// ---------------------------------------------------------------------------------------------------------------
// recent repositories (07 §4.4)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn recent_filter_is_a_case_insensitive_substring_of_the_full_path() {
    let entries: Vec<PathBuf> = ["/work/Alpha", "/work/beta", "/play/alphabet"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(recent_repos::filter_recent(&entries, ""), [0, 1, 2]);
    assert_eq!(recent_repos::filter_recent(&entries, "ALP"), [0, 2]);
    assert_eq!(
        recent_repos::filter_recent(&entries, "play"),
        [2],
        "matches the parent path too"
    );
    assert!(recent_repos::filter_recent(&entries, "zzz").is_empty());
}

#[test]
fn recent_row_texts() {
    let home = Path::new("/home/me");
    assert_eq!(
        recent_repos::repository_name(Path::new("/home/me/src/repo")),
        "repo"
    );
    assert_eq!(
        recent_repos::parent_display(Path::new("/home/me/src/repo"), Some(home)),
        format!("~{}src", std::path::MAIN_SEPARATOR)
    );
    assert_eq!(
        recent_repos::parent_display(Path::new("/home/me/repo"), Some(home)),
        "~"
    );
    let opt = Path::new("/opt").display().to_string();
    assert_eq!(
        recent_repos::parent_display(Path::new("/opt/repo"), Some(home)),
        opt
    );
    assert_eq!(
        recent_repos::parent_display(Path::new("/opt/repo"), None),
        opt
    );
}

struct RecentProbe {
    window: AnyWindowHandle,
    recent: Rc<RefCell<Option<Entity<RecentRepos>>>>,
    opened: Rc<RefCell<Vec<PathBuf>>>,
    exits: Rc<Cell<usize>>,
    _subscription: Rc<RefCell<Option<gpui_kit::Subscription>>>,
}

fn recent_probe(cx: &mut TestAppContext, entries: &[&str]) -> RecentProbe {
    init_app(cx);
    set_recent(cx, entries.iter().map(PathBuf::from).collect());
    let slot: Rc<RefCell<Option<Entity<RecentRepos>>>> = Rc::default();
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let exits = Rc::new(Cell::new(0));
    let subscription: Rc<RefCell<Option<gpui_kit::Subscription>>> = Rc::default();
    let (build_slot, build_opened, build_exits, build_sub) = (
        slot.clone(),
        opened.clone(),
        exits.clone(),
        subscription.clone(),
    );
    let (window, _probe) = open_probe(cx, 600., 400., move |window, cx| {
        let recent = build_slot
            .borrow_mut()
            .get_or_insert_with(|| {
                let sink = build_opened.clone();
                let recent = cx.new(|cx| {
                    RecentRepos::new(
                        Rc::new(move |path, _, _| sink.borrow_mut().push(path)),
                        window,
                        cx,
                    )
                });
                let exits = build_exits.clone();
                *build_sub.borrow_mut() =
                    Some(cx.subscribe(&recent, move |_, _, _: &RecentReposEvent, _| {
                        exits.set(exits.get() + 1)
                    }));
                recent
            })
            .clone();
        recent.into_any_element()
    });
    RecentProbe {
        window,
        recent: slot,
        opened,
        exits,
        _subscription: subscription,
    }
}

impl RecentProbe {
    fn recent(&self) -> Entity<RecentRepos> {
        self.recent.borrow().clone().expect("the probe rendered")
    }
}

fn shown(cx: &mut TestAppContext, probe: &RecentProbe) -> Vec<String> {
    let recent = probe.recent();
    cx.update(|cx| {
        recent
            .read(cx)
            .shown()
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    })
}

#[gpui_kit::test]
fn recent_list_filter_keyboard(cx: &mut TestAppContext) {
    let probe = recent_probe(cx, &["/work/alpha", "/work/beta", "/play/alphabet"]);
    frame(cx, probe.window, |_, _| ());
    assert_eq!(shown(cx, &probe).len(), 3);
    // Row geometry: 40 high, below the filter field (38) and the 4 dp gap.
    frame(cx, probe.window, |window, _| {
        let row = bounds(window, (ids::RECENT_ROW, 0usize));
        assert!((row.size.height.as_f32() - 40.).abs() <= 0.5);
        assert!(
            (row.origin.y.as_f32() - (38. + dims::RECENT_LIST_PAD_TOP)).abs() <= 0.5,
            "the first row starts below the field and 4 dp"
        );
    });
    let recent = probe.recent();
    cx.update_window(probe.window, |_, window, cx| {
        recent.update(cx, |r, cx| r.focus_search(window, cx));
        window.render_frame(cx);
        window.input("ALP", cx);
        window.render_frame(cx);
    })
    .expect("window");
    assert_eq!(
        shown(cx, &probe),
        [
            Path::new("/work/alpha").display().to_string(),
            Path::new("/play/alphabet").display().to_string()
        ],
        "typing filters case-insensitively"
    );
    assert_eq!(
        cx.update(|cx| recent.read(cx).focused_row()),
        0,
        "typing resets the highlight"
    );
    let press = |cx: &mut TestAppContext, key: &'static str| {
        cx.update_window(probe.window, |_, window, cx| window.press(key, cx))
            .expect("window");
    };
    press(cx, "down");
    assert_eq!(
        cx.update(|cx| recent.read(cx).focused_row()),
        1,
        "Down moves the highlight"
    );
    press(cx, "down");
    assert_eq!(
        cx.update(|cx| recent.read(cx).focused_row()),
        1,
        "Down stops at the last row"
    );
    press(cx, "up");
    assert_eq!(
        cx.update(|cx| recent.read(cx).focused_row()),
        0,
        "Up moves back"
    );
    press(cx, "down");
    press(cx, "enter");
    assert_eq!(
        *probe.opened.borrow(),
        [PathBuf::from("/play/alphabet")],
        "Enter opens the highlighted repository"
    );
    // Escape clears the filter first...
    press(cx, "escape");
    assert_eq!(shown(cx, &probe).len(), 3, "Escape cleared the filter");
    assert_eq!(probe.exits.get(), 0);
    // ...and with an empty filter reports the exit.
    press(cx, "escape");
    assert_eq!(
        probe.exits.get(),
        1,
        "Escape on an empty filter is reported"
    );
}

#[gpui_kit::test]
fn recent_clear_button_and_hover_highlight(cx: &mut TestAppContext) {
    let probe = recent_probe(cx, &["/work/alpha", "/work/beta", "/play/gamma"]);
    frame(cx, probe.window, |_, _| ());
    let recent = probe.recent();
    cx.update_window(probe.window, |_, window, cx| {
        recent.update(cx, |r, cx| r.focus_search(window, cx));
        window.render_frame(cx);
        assert!(
            window.try_find(ids::RECENT_FILTER_CLEAR).is_none(),
            "no clear button for an empty filter"
        );
        window.input("work", cx);
    })
    .expect("window");
    cx.update_window(probe.window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(ids::RECENT_FILTER_CLEAR).is_some());
        window.hover((ids::RECENT_ROW, 1usize), cx);
        window.render_frame(cx);
    })
    .expect("window");
    assert_eq!(
        cx.update(|cx| recent.read(cx).focused_row()),
        1,
        "the highlight follows the mouse while the field has focus"
    );
    cx.update_window(probe.window, |_, window, cx| {
        window.click(ids::RECENT_FILTER_CLEAR, cx)
    })
    .expect("window");
    assert_eq!(
        shown(cx, &probe).len(),
        3,
        "the clear button cleared the filter"
    );
}

#[gpui_kit::test]
fn recent_remove_button_forgets_the_repository_without_opening_it(cx: &mut TestAppContext) {
    let probe = recent_probe(cx, &["/work/alpha", "/work/beta"]);
    frame(cx, probe.window, |_, _| ());
    cx.update_window(probe.window, |_, window, cx| {
        // The button appears while the row is hovered.
        window.hover((ids::RECENT_ROW, 0usize), cx);
        window.render_frame(cx);
        window.click((ids::RECENT_REMOVE, 0usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    assert!(probe.opened.borrow().is_empty(), "removing does not open");
    assert_eq!(
        shown(cx, &probe),
        [Path::new("/work/beta").display().to_string()]
    );
    cx.update(|cx| {
        assert_eq!(
            state::get(cx).recent_repositories,
            [PathBuf::from("/work/beta")],
            "the UI state forgot it"
        );
    });
}

#[gpui_kit::test]
fn recent_list_is_virtualised(cx: &mut TestAppContext) {
    let entries: Vec<String> = (0..100).map(|n| format!("/work/repo-{n:03}")).collect();
    let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
    let probe = recent_probe(cx, &refs);
    frame(cx, probe.window, |window, _| {
        assert!(window.try_find((ids::RECENT_ROW, 0usize)).is_some());
        assert!(
            window.try_find((ids::RECENT_ROW, 60usize)).is_none(),
            "rows outside the viewport are not built"
        );
    });
}

// ---------------------------------------------------------------------------------------------------------------
// bottom bar (07 §7)
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn identity_label_uses_effective_then_configured_parts() {
    let effective = IdentityInfo {
        global: PartialIdentity::default(),
        local: PartialIdentity::default(),
        effective: Some(Identity {
            name: "Ada".into(),
            email: "ada@example.org".into(),
        }),
    };
    assert_eq!(
        bottom_bar::identity_label(&effective),
        "Ada <ada@example.org>"
    );
    let nothing = IdentityInfo {
        effective: None,
        ..effective
    };
    assert_eq!(
        bottom_bar::identity_label(&nothing),
        format!("{NAME_NOT_SET} <{EMAIL_NOT_SET}>")
    );
    let partial = IdentityInfo {
        effective: None,
        global: PartialIdentity {
            name: Some("Global".into()),
            email: None,
        },
        local: PartialIdentity {
            name: None,
            email: Some("local@example.org".into()),
        },
    };
    assert_eq!(
        bottom_bar::identity_label(&partial),
        "Global <local@example.org>",
        "each part comes from local, else global"
    );
}

#[gpui_kit::test]
fn bottom_bar_identity_click_opens_author(cx: &mut TestAppContext) {
    let (handle, app, _tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        let identity = bounds(window, ids::BOTTOM_IDENTITY);
        assert!(
            (identity.origin.x.as_f32() - 16.).abs() <= 0.5,
            "the identity starts 16 dp from the edge"
        );
        assert!((identity.size.height.as_f32() - 32.).abs() <= 0.5);
        let version = bounds(window, ids::BOTTOM_VERSION);
        assert!(
            (version.right().as_f32() - (1280. - 16.)).abs() <= 0.5,
            "the version link ends 16 dp from the edge"
        );
    });
    frame(cx, handle, |window, cx| {
        window.click(ids::BOTTOM_IDENTITY, cx)
    });
    cx.run_until_parked();
    frame(cx, handle, |window, cx| {
        assert!(
            dialog_is_open(&app, window, cx),
            "the identity opens the Author dialog"
        );
    });
}

#[gpui_kit::test]
fn bottom_bar_lays_out_update_slot_before_the_version_link(cx: &mut TestAppContext) {
    init_app(cx);
    let recorder = Recorder::default();
    recorder.install(cx);
    let (window, _probe) = open_probe(cx, 1280., 32., |_, cx| {
        let tokens = *cx.tokens();
        let slot = observed("slot-probe")
            .w(dp(100.))
            .h(dp(24.))
            .into_any_element();
        bottom_bar::bar(tokens, None, Some(slot)).into_any_element()
    });
    frame(cx, window, |window, cx| {
        let slot = bounds(window, "slot-probe");
        let version = bounds(window, ids::BOTTOM_VERSION);
        assert!(
            (version.right().as_f32() - 1264.).abs() <= 0.5,
            "the version link ends 16 dp from the edge"
        );
        assert!(
            (version.left().as_f32() - slot.right().as_f32() - 16.).abs() <= 0.5,
            "the update slot ends 16 dp before the version link"
        );
        window.click(ids::BOTTOM_VERSION, cx);
    });
    assert_eq!(
        *recorder.urls.borrow(),
        [crate::update::RELEASES_URL.to_owned()]
    );
    assert_eq!(
        bottom_bar::version_label(),
        format!("Version {}", env!("CARGO_PKG_VERSION"))
    );
}

// ---------------------------------------------------------------------------------------------------------------
// processing overlay and loading screen (07 §3)
// ---------------------------------------------------------------------------------------------------------------

fn busy(kind: OpKind, phase: Option<&str>) -> BusyState {
    BusyState {
        detail: None,
        kind,
        cancel: crate::backend::CancelToken::new(),
        cancellable: Cancellable::Yes,
        started: std::time::Instant::now(),
        overlay_visible: true,
        phase: phase.map(|p| p.to_owned().into()),
    }
}

#[gpui_kit::test]
fn processing_overlay_layout_and_cancel(cx: &mut TestAppContext) {
    init_app(cx);
    let cancelled = Rc::new(Cell::new(0));
    let sink = cancelled.clone();
    let state = Rc::new(RefCell::new((
        busy(OpKind::Fetch, Some("Receiving objects 42%")),
        true,
    )));
    let shared = state.clone();
    let (window, _probe) = open_probe(cx, 1280., 684., move |_, cx| {
        let (busy, with_cancel) = &*shared.borrow();
        let sink = sink.clone();
        let on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App)>> = with_cancel
            .then(|| Rc::new(move |_: &mut Window, _: &mut App| sink.set(sink.get() + 1)) as _);
        super::loading::processing_overlay(busy, on_cancel, cx)
    });
    frame(cx, window, |window, cx| {
        assert_rect(window, ids::PROCESSING, 0., 0., 1280., 684.);
        let title = bounds(window, ids::PROCESSING_TITLE);
        let subtitle = bounds(window, ids::PROCESSING_SUBTITLE);
        let bar = bounds(window, ids::PROCESSING_BAR);
        let phase = bounds(window, ids::PROCESSING_PHASE);
        let cancel = bounds(window, ids::PROCESSING_CANCEL);
        // Title, subtitle, bar, phase and Cancel are stacked top to bottom with the spec's paddings.
        assert!(
            (subtitle.top().as_f32() - title.bottom().as_f32() - 8.).abs() <= 0.5,
            "8 dp below the title"
        );
        assert!(
            (bar.top().as_f32() - subtitle.bottom().as_f32() - 32.).abs() <= 0.5,
            "32 dp below the subtitle"
        );
        assert!(
            (bar.size.width.as_f32() - 280.).abs() <= 0.5,
            "the bar is 280 wide"
        );
        assert!(
            (phase.top().as_f32() - bar.bottom().as_f32() - 32.).abs() <= 0.5,
            "32 dp below the bar"
        );
        assert!(
            (cancel.top().as_f32() - phase.bottom().as_f32() - 16.).abs() <= 0.5,
            "16 dp between the phase line and Cancel"
        );
        for b in [title, subtitle, bar, phase, cancel] {
            assert!(
                (b.center().x.as_f32() - 640.).abs() <= 1.,
                "{b:?} is centred"
            );
        }
        window.click(ids::PROCESSING_CANCEL, cx);
    });
    assert_eq!(cancelled.get(), 1, "Cancel calls the callback");

    // A non-cancellable operation without progress shows neither the phase line nor Cancel.
    *state.borrow_mut() = (busy(OpKind::Pull, None), false);
    frame(cx, window, |window, _| {
        assert!(window.try_find(ids::PROCESSING_PHASE).is_none());
        assert!(window.try_find(ids::PROCESSING_CANCEL).is_none());
        assert!(window.try_find(ids::PROCESSING_BAR).is_some());
    });
    assert_eq!(
        super::loading::processing_subtitle(OpKind::Pull),
        "Pulling changes from remote branch to the current"
    );
    assert_eq!(
        super::loading::processing_subtitle(OpKind::Fetch),
        "This could take a few seconds"
    );
}

fn noop_outcome() -> crate::backend::OpOutcome<()> {
    crate::backend::OpOutcome {
        value: (),
        refresh: crate::backend::RefreshSet::empty(),
        log: Default::default(),
        warnings: Vec::new(),
    }
}

#[gpui_kit::test]
fn overlay_cancel_button_cancels_the_running_operation(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    cx.update(|cx| {
        crate::test_support::script_op(&tab, cx, Ok(noop_outcome()), Duration::from_secs(5))
    });
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| {
            t.run_op(
                OpSpec::new(OpKind::Fetch, |_| Ok(noop_outcome())).cancellable(Cancellable::Yes),
                window,
                cx,
            )
        })
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_millis(301));
    cx.run_until_parked();
    frame(cx, handle, |window, cx| {
        assert!(
            window.try_find(ids::PROCESSING).is_some(),
            "the overlay is visible after 300 ms"
        );
        window.click(ids::PROCESSING_CANCEL, cx);
    });
    cx.update(|cx| {
        assert!(
            tab.read(cx).busy().is_some_and(|b| b.cancel.is_cancelled()),
            "the Cancel link cancelled the operation's token"
        );
    });
}

#[gpui_kit::test]
fn loading_screen_fills_the_probe(cx: &mut TestAppContext) {
    init_app(cx);
    let (window, _probe) = open_probe(cx, 800., 600., |_, cx| {
        super::loading::loading_repository(Path::new("/work/repo"), cx)
    });
    frame(cx, window, |window, _| {
        assert_rect(window, ids::LOADING, 0., 0., 800., 600.);
        // 36 sp title over a 24 sp path (07 §3.1), line height 1.21 x size, centred as a column.
        let title = bounds(window, ids::LOADING_TITLE);
        let path = bounds(window, ids::LOADING_PATH);
        assert!(
            (title.size.height.as_f32() - 36. * 1.21).abs() <= 1.,
            "title line {title:?}"
        );
        assert!(
            (path.size.height.as_f32() - 24. * 1.21).abs() <= 1.,
            "path line {path:?}"
        );
        assert!((path.top().as_f32() - title.bottom().as_f32()).abs() <= 0.5);
        let middle = (title.top().as_f32() + path.bottom().as_f32()) / 2.;
        assert!((middle - 300.).abs() <= 1., "vertically centred");
        assert!((title.center().x.as_f32() - 400.).abs() <= 1.);
    });
}

#[gpui_kit::test]
fn welcome_clear_button_keeps_the_focus_in_the_search_field(cx: &mut TestAppContext) {
    init_app(cx);
    set_recent(
        cx,
        vec![PathBuf::from("/work/alpha"), PathBuf::from("/work/beta")],
    );
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |_, _| ());
    cx.run_until_parked();
    let welcome = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.clone(),
        _ => panic!("a welcome tab"),
    });
    cx.update_window(handle, |_, window, cx| window.input("alp", cx))
        .expect("window");
    frame(cx, handle, |window, cx| {
        assert_eq!(welcome.read(cx).recent().read(cx).shown().len(), 1);
        window.click(ids::RECENT_FILTER_CLEAR, cx);
    });
    frame(cx, handle, |window, cx| {
        let recent = welcome.read(cx).recent().read(cx);
        assert_eq!(recent.shown().len(), 2, "the filter was cleared");
        assert!(
            recent.search_focused(window, cx),
            "the click on the clear button did not move the focus to the page"
        );
    });
    // Removing a repository does not take the focus from the field either.
    cx.update_window(handle, |_, window, cx| {
        window.hover((ids::RECENT_ROW, 0usize), cx);
        window.render_frame(cx);
        window.click((ids::RECENT_REMOVE, 0usize), cx);
    })
    .expect("window");
    cx.run_until_parked();
    frame(cx, handle, |window, cx| {
        let recent = welcome.read(cx).recent().read(cx);
        assert_eq!(recent.shown(), [PathBuf::from("/work/beta")]);
        assert!(recent.search_focused(window, cx));
    });
}

#[gpui_kit::test]
fn welcome_click_on_an_empty_area_keeps_the_focus_in_the_search_field(cx: &mut TestAppContext) {
    init_app(cx);
    set_recent(cx, vec![PathBuf::from("/work/alpha")]);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |_, _| ());
    cx.run_until_parked();
    let welcome = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.clone(),
        _ => panic!("a welcome tab"),
    });
    frame(cx, handle, |window, cx| {
        window.click(ids::WELCOME_TITLE, cx)
    });
    frame(cx, handle, |window, cx| {
        assert!(
            welcome
                .read(cx)
                .recent()
                .read(cx)
                .search_focused(window, cx),
            "a press on the page's title left the search field focused"
        );
    });
}

#[gpui_kit::test]
fn a_failed_open_returns_to_a_welcome_page_and_the_error_keeps_the_focus(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let not_a_repo = fx.dir.path().join("plain");
    std::fs::create_dir_all(&not_a_repo).expect("directory");
    init_app(cx);
    git_ready(cx, &fx);
    set_recent(cx, vec![PathBuf::from("/work/alpha")]);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |_, _| ());
    cx.run_until_parked();
    let first = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.entity_id(),
        _ => panic!("a welcome tab"),
    });
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.open_repository(not_a_repo.clone(), true, window, cx)
        })
    })
    .expect("window");
    cx.run_until_parked();
    frame(cx, handle, |_, _| ());
    cx.run_until_parked();
    let welcome = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.clone(),
        _ => panic!("the slot fell back to a welcome page"),
    });
    assert_ne!(welcome.entity_id(), first, "a new welcome page");
    frame(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx), "the open failure is shown");
        assert!(
            !welcome.read(cx).is_focused(window, cx),
            "the new welcome page does not steal the dialog's focus"
        );
    });
}

#[gpui_kit::test]
fn welcome_focuses_the_search_field_or_the_page(cx: &mut TestAppContext) {
    // With recent repositories the search field gets the focus (07 §4.2)...
    init_app(cx);
    set_recent(cx, vec![PathBuf::from("/work/alpha")]);
    let (handle, app) = open_app(cx, DEFAULT_SIZE);
    frame(cx, handle, |_, _| ());
    cx.run_until_parked();
    let welcome = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.clone(),
        _ => panic!("a welcome tab"),
    });
    frame(cx, handle, |window, cx| {
        let page = welcome.read(cx);
        assert!(page.is_focused(window, cx));
        assert!(page.recent().read(cx).search_focused(window, cx));
    });
    // ...without them the page itself.
    let (handle, app) = {
        set_recent(cx, Vec::new());
        let (handle, app) = open_app(cx, DEFAULT_SIZE);
        frame(cx, handle, |_, _| ());
        cx.run_until_parked();
        (handle, app)
    };
    let welcome = cx.update(|cx| match &app.read(cx).tabs()[0].content {
        TabContent::Welcome(view) => view.clone(),
        _ => panic!("a welcome tab"),
    });
    frame(cx, handle, |window, cx| {
        let page = welcome.read(cx);
        assert!(page.is_focused(window, cx));
        assert!(!page.recent().read(cx).search_focused(window, cx));
    });
}

/// The Pull overlay names the upstream: the current branch's, or the remote branch that was asked for.
#[gpui_kit::test]
fn pull_overlay_names_the_upstream(cx: &mut TestAppContext) {
    let mut seed = RepoSeed::branchy();
    let head = match &seed.refs.head {
        crate::backend::HeadState::Branch { name, .. } => name.clone(),
        other => panic!("the fixture has a branch checked out: {other:?}"),
    };
    let current = seed
        .refs
        .local
        .iter_mut()
        .find(|b| b.full == head)
        .expect("the current branch is listed");
    current.upstream = Some(crate::backend::Upstream {
        remote: "origin".into(),
        merge: crate::test_support::seed::full("refs/heads/trunk"),
        tracking: Some(crate::test_support::seed::full("refs/remotes/origin/trunk")),
        gone: false,
    });
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    cx.update(|cx| {
        crate::test_support::script_op(&tab, cx, Ok(noop_outcome()), Duration::from_secs(5))
    });
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| t.pull(None, window, cx))
    })
    .expect("window");
    cx.update(|cx| {
        let detail = tab.read(cx).busy().and_then(|b| b.detail.clone());
        assert_eq!(
            detail.as_deref(),
            Some("Pulling changes from origin/trunk to the current"),
            "a plain pull names the branch's upstream"
        );
    });
}
