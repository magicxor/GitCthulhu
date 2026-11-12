use core::prelude::v1::test;

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, ElementId, Entity, TestAppContext, hsla};

use super::ids;
use super::*;
use crate::backend::ChangeKind;
use crate::ids as app_ids;
use crate::menus::MenuItem;
use crate::tab::RepoTab;
use crate::tab::ops::OpKind;
use crate::terminal::TerminalButtonState;
use crate::test_support::geometry::{assert_rect, bounds, init_visual, open_probe};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, inert_tab_ctx, mount_fixture, mount_seeded,
    toast_texts,
};

/// Top of the toolbar's buttons: below the tab strip plus the 4 dp padding.
const BUTTON_Y: f32 = metrics::TAB_STRIP_H + metrics::TOOLBAR_PAD_V;

fn ctx_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> TabCtx {
    cx.update(|cx| tab.read(cx).ctx().clone())
}

fn chevron(id: &'static str) -> ElementId {
    ElementId::from((ElementId::from(id), "chevron"))
}

fn ops(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<OpKind> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|call| match call {
                TestCall::Op { kind, .. } => Some(kind),
                _ => None,
            })
            .collect()
    })
}

/// The Stash button or Ctrl+S ran the Stash operation (a "Not implemented yet" toast no longer stands in for it).
fn stash_requested(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> bool {
    ops(tab, cx).contains(&OpKind::Stash)
}

fn with_changes() -> RepoSeed {
    RepoSeed::branchy().with_changes(&[], &[("a.txt", ChangeKind::Modified)])
}

/// Renders one frame of the window, runs `f`, and lets the effects settle.
fn frame<T>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> T,
) -> T {
    let result = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
        .expect("window");
    cx.run_until_parked();
    result
}

#[gpui_kit::test]
fn toolbar_geometry(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, _| {
        assert_rect(
            window,
            app_ids::TOOLBAR,
            0.,
            metrics::TAB_STRIP_H,
            1280.,
            64.,
        );
        assert_rect(window, ids::OPEN, 16., BUTTON_Y, 56., 56.);

        // Left group 72, right group 192; the 532 dp central group (376 + Undo 64 + 4 + Redo 56 + 32 spacer)
        // sits between them.
        let group_x = 72. + (1280. - 72. - 192. - 532.) / 2.;
        assert_rect(window, ids::CENTER_GROUP, group_x, BUTTON_Y, 532., 56.);
        let gitnuro_pull_x = 72. + (1280. - 72. - 192. - 376.) / 2.;
        let pull_x = group_x + 64. + 4. + 56. + 32.;
        assert!(
            (pull_x - gitnuro_pull_x - 78.).abs() < 0.01,
            "Pull is 78 dp right of Gitnuro's position"
        );

        assert_rect(
            window,
            crate::undo::ids::TOOLBAR_UNDO,
            group_x,
            BUTTON_Y,
            64.,
            56.,
        );
        assert_rect(
            window,
            crate::undo::ids::TOOLBAR_REDO,
            group_x + 68.,
            BUTTON_Y,
            56.,
            56.,
        );
        assert_rect(window, ids::PULL, pull_x, BUTTON_Y, 64., 56.);
        assert_rect(window, ids::PUSH, pull_x + 68., BUTTON_Y, 64., 56.);
        let branch_x = pull_x + 68. + 64. + 32.;
        assert_rect(window, ids::BRANCH, branch_x, BUTTON_Y, 56., 56.);
        let stash_x = branch_x + 56. + 32.;
        assert_rect(window, ids::STASH, stash_x, BUTTON_Y, 64., 56.);
        assert_rect(window, ids::POP_STASH, stash_x + 68., BUTTON_Y, 56., 56.);
        assert!(
            (stash_x + 68. + 56. - (group_x + 532.)).abs() < 0.01,
            "the group ends after Pop stash"
        );

        // Chevron strips: 16 wide, the right end of each split button.
        for (id, x) in [
            (ids::PULL, pull_x),
            (ids::PUSH, pull_x + 68.),
            (ids::STASH, stash_x),
        ] {
            assert_rect(window, chevron(id), x + 48., BUTTON_Y, 16., 56.);
        }

        // Right group: Terminal and Actions with 4 dp margins, Settings with 16 dp at the window's edge.
        assert_rect(window, ids::RIGHT_GROUP, 1280. - 192., BUTTON_Y, 192., 56.);
        assert_rect(window, ids::TERMINAL, 1280. - 192., BUTTON_Y, 56., 56.);
        assert_rect(window, ids::ACTIONS, 1280. - 192. + 60., BUTTON_Y, 56., 56.);
        assert_rect(window, ids::SETTINGS, 1280. - 16. - 56., BUTTON_Y, 56., 56.);
    });
}

#[gpui_kit::test]
fn central_group_stays_between_the_left_and_right_groups_when_narrow(cx: &mut TestAppContext) {
    // 796 dp is the minimum width without overlap (21 §5.1).
    let (handle, _app, _tab) = mount_seeded(
        cx,
        with_changes(),
        crate::test_support::window_size(796., 720.),
    );
    frame(cx, handle, |window, _| {
        let group = bounds(window, ids::CENTER_GROUP);
        let open = bounds(window, ids::OPEN);
        let terminal = bounds(window, ids::TERMINAL);
        assert!(
            group.left() >= open.right() - gpui_kit::px(0.5),
            "no overlap with Open"
        );
        assert!(
            group.right() <= terminal.left() + gpui_kit::px(0.5),
            "no overlap with the right group"
        );
    });
}

#[gpui_kit::test]
fn pull_push_disabled_without_remote(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        window.click(ids::PULL, cx);
        window.click(ids::PUSH, cx);
    });
    assert!(ops(&tab, cx).is_empty(), "disabled buttons do nothing");
    frame(cx, handle, |window, cx| {
        window.click(chevron(ids::PULL), cx);
        window.render_frame(cx);
        assert!(
            window.try_find(app_ids::MENU).is_none(),
            "the chevron of a disabled split button opens nothing"
        );
    });
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let state = ToolbarState::read(model, &crate::settings::get_or_default(cx));
        assert!(!state.has_remote);
        assert_eq!(
            state.pull_tooltip(),
            "Pull not available without a remote repository"
        );
        assert_eq!(
            state.push_tooltip(),
            "Push not available without a remote repository"
        );
    });

    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(ids::PULL, cx));
    assert_eq!(ops(&tab, cx), [OpKind::Pull], "an enabled Pull runs");
}

#[gpui_kit::test]
fn buttons_run_their_operation(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(ids::PULL, cx));
    frame(cx, handle, |window, cx| window.click(ids::PUSH, cx));
    frame(cx, handle, |window, cx| window.click(ids::STASH, cx));
    frame(cx, handle, |window, cx| window.click(ids::POP_STASH, cx));
    let ran = ops(&tab, cx);
    assert!(ran.contains(&OpKind::Pull), "Pull: {ran:?}");
    assert!(ran.contains(&OpKind::Push), "Push: {ran:?}");
    assert!(ran.contains(&OpKind::PopStash), "Pop stash: {ran:?}");
    assert!(
        stash_requested(&tab, cx),
        "Stash: {ran:?} {:?}",
        cx.update(|cx| toast_texts(&app, cx))
    );
}

#[gpui_kit::test]
fn stash_disabled_when_clean(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        window.click(ids::STASH, cx);
        window.click(ids::POP_STASH, cx);
    });
    assert!(
        ops(&tab, cx).is_empty(),
        "nothing to stash and no stash to pop"
    );
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let state = ToolbarState::read(model, &crate::settings::get_or_default(cx));
        assert!(!state.can_stash && !state.has_stash);
        assert_eq!(
            state.stash_tooltip(),
            "Change some files to be able to stash them"
        );
        assert_eq!(state.pop_tooltip(), "No stashes to pop");
    });

    // A clean seeded repository with a stash can pop it but not stash.
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(ids::STASH, cx));
    assert!(ops(&tab, cx).is_empty(), "a clean tree cannot be stashed");
    frame(cx, handle, |window, cx| window.click(ids::POP_STASH, cx));
    assert_eq!(ops(&tab, cx), [OpKind::PopStash]);
}

#[gpui_kit::test]
fn dropdown_anchored_at_chevron(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        let strip = bounds(window, chevron(ids::PULL));
        window.click(chevron(ids::PULL), cx);
        window.render_frame(cx);
        let menu = bounds(window, app_ids::MENU);
        assert!(
            (menu.origin.x - strip.left()).as_f32().abs() <= 0.5
                && (menu.origin.y - strip.bottom()).as_f32().abs() <= 0.5,
            "the dropdown starts at the chevron strip's bottom-left: menu {menu:?}, strip {strip:?}"
        );
        assert!(
            window.try_find("menu-item-pull-opposite").is_some()
                && window.try_find("menu-item-fetch-all").is_some(),
            "Pull offers the opposite pull type and Fetch all"
        );
        window.click("menu-item-pull-opposite", cx);
    });
    assert_eq!(ops(&tab, cx), [OpKind::Pull]);
    frame(cx, handle, |window, cx| {
        window.click(chevron(ids::PUSH), cx);
        window.render_frame(cx);
        assert!(window.try_find("menu-item-push-tags").is_some());
        assert!(window.try_find("menu-item-push-force").is_some());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find(app_ids::MENU).is_none(), "Escape closes it");
    });
}

#[gpui_kit::test]
fn pull_menu_offers_the_opposite_of_the_default(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    let labels = |cx: &mut TestAppContext| -> Vec<String> {
        cx.update(|cx| {
            crate::menus::toolbar::pull_menu(&ctx, cx)
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Action { label, .. } => Some(label.to_string()),
                    MenuItem::Separator => None,
                })
                .collect()
        })
    };
    assert_eq!(labels(cx), ["Pull with rebase", "Fetch all"]);
    cx.update(|cx| crate::settings::update(cx, |s| s.pull_with_rebase = true));
    assert_eq!(labels(cx), ["Pull with merge", "Fetch all"]);
    cx.update(|cx| {
        let model = ctx.model.read(cx);
        let state = ToolbarState::read(model, &crate::settings::get_or_default(cx));
        assert_eq!(state.pull_tooltip(), "Pull current branch with rebase");
    });

    let kinds = |items: Vec<MenuItem>| -> Vec<(String, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { label, icon, .. } => Some((label.to_string(), icon.is_some())),
                MenuItem::Separator => None,
            })
            .collect()
    };
    cx.update(|cx| {
        assert_eq!(
            kinds(crate::menus::toolbar::push_menu(&ctx, cx)),
            [
                ("Push including tags".to_owned(), true),
                ("Force push".to_owned(), false)
            ]
        );
        assert_eq!(
            kinds(crate::menus::toolbar::stash_menu(&ctx, cx)),
            [("Stash with message".to_owned(), true)]
        );
    });
}

#[gpui_kit::test]
fn shortcuts_dispatch_actions(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    // A press inside the frame gives it the focus; the keys are bound in its `RepoView` context.
    frame(cx, handle, |window, cx| {
        window.click(crate::side_panel::ids::SIDE_PANEL, cx)
    });
    frame(cx, handle, |window, cx| window.press("ctrl-s", cx));
    assert!(stash_requested(&tab, cx), "Ctrl+S stashes");
    for (key, kind) in [
        ("ctrl-u", OpKind::Pull),
        ("ctrl-p", OpKind::Push),
        ("ctrl-shift-s", OpKind::PopStash),
    ] {
        frame(cx, handle, |window, cx| window.press(key, cx));
        let ran = ops(&tab, cx);
        assert_eq!(ran.last(), Some(&kind), "{key} runs {kind:?}: {ran:?}");
    }
    frame(cx, handle, |window, cx| window.press("f5", cx));
    let reloaded = cx.update(|cx| {
        calls(&tab, cx)
            .iter()
            .any(|c| matches!(c, TestCall::Reload(set) if *set == crate::backend::RefreshSet::ALL))
    });
    assert!(reloaded, "F5 reloads everything");
}

#[gpui_kit::test]
fn open_popup_600x600_rounded(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle, |window, cx| {
        open_popup::open(&ctx, window, cx);
    });
    frame(cx, handle, |window, _| {
        // x = left of the Open button (16), y = its bottom (tab strip 36 + 4 + 56).
        assert_rect(
            window,
            ids::OPEN_POPUP,
            16.,
            BUTTON_Y + metrics::MENU_BUTTON,
            600.,
            600.,
        );
        // "Open a repository" sits 16 dp inside the popup at its own width (no fillMaxWidth in Gitnuro).
        let popup_y = BUTTON_Y + metrics::MENU_BUTTON;
        let pick = bounds(window, ids::OPEN_POPUP_PICK);
        assert!(
            (pick.origin.x.as_f32() - 32.).abs() <= 0.5
                && (pick.origin.y.as_f32() - (popup_y + 16.)).abs() <= 0.5,
            "pick button at the popup's padding: {pick:?}"
        );
        assert!(
            pick.size.width.as_f32() < 600. - 32. - 100.,
            "the button is as wide as its text: {pick:?}"
        );
    });
    // The popup's background follows its rounded border (deviation 16).
    assert_eq!(metrics::RADIUS_LG, 8.);
    assert_eq!(open_popup::POPUP_BORDER, 2.);

    frame(cx, handle, |window, cx| {
        window.press("escape", cx);
    });
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::OPEN_POPUP).is_none(),
            "Escape closes the popup"
        );
    });

    frame(cx, handle, |window, cx| open_popup::open(&ctx, window, cx));
    frame(cx, handle, |window, cx| {
        assert!(window.try_find(ids::OPEN_POPUP).is_some());
        // The backdrop covers the window: a press anywhere outside the popup dismisses it.
        window.click(app_ids::BOTTOM_BAR, cx);
    });
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::OPEN_POPUP).is_none(),
            "clicking outside dismisses the popup"
        );
    });
}

#[gpui_kit::test]
fn open_action_in_a_repository_opens_the_popup(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    let _ = tab;
    frame(cx, handle, |window, cx| {
        window.click(crate::side_panel::ids::SIDE_PANEL, cx)
    });
    frame(cx, handle, |window, cx| window.press("ctrl-o", cx));
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::OPEN_POPUP).is_some(),
            "Ctrl+O opens the popup in a repository tab"
        );
    });
    frame(cx, handle, |window, cx| window.click(ids::OPEN, cx));
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::OPEN_POPUP).is_none(),
            "the backdrop swallowed the click on Open and dismissed the popup"
        );
    });
}

#[gpui_kit::test]
fn open_replaces_the_current_tab(cx: &mut TestAppContext) {
    let first = Fixture::new();
    first.write("a.txt", "one\n");
    first.commit_all("first");
    let second = Fixture::new();
    second.write("b.txt", "two\n");
    second.commit_all("second");
    let (handle, app, tab) = mount_fixture(cx, &first, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    cx.update(|cx| {
        platform::install_fake(cx, Some(Some(PathBuf::from(second.path()))));
    });
    frame(cx, handle, |window, cx| open_popup::open(&ctx, window, cx));
    frame(cx, handle, |window, cx| {
        window.click(ids::OPEN_POPUP_PICK, cx)
    });
    for _ in 0..20 {
        cx.run_until_parked();
    }
    frame(cx, handle, |_, _| ());
    cx.update(|cx| {
        assert_eq!(platform::calls(cx), ["pick"], "the directory picker ran");
        let view = app.read(cx);
        assert_eq!(view.tabs().len(), 1, "the tab was replaced, not added");
        let path = view.tabs()[0].path(cx).expect("the new tab has a path");
        assert_eq!(
            path.canonicalize().ok(),
            second.path().canonicalize().ok(),
            "the slot shows the picked repository"
        );
    });
}

#[gpui_kit::test]
fn recent_list_choice_replaces_the_current_tab(cx: &mut TestAppContext) {
    let first = Fixture::new();
    first.write("a.txt", "one\n");
    first.commit_all("first");
    let second = Fixture::new();
    second.write("b.txt", "two\n");
    second.commit_all("second");
    let (handle, app, tab) = mount_fixture(cx, &first, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle, |window, cx| open_popup::open(&ctx, window, cx));
    let on_open = cx.update(|cx| {
        let toolbar = toolbar_of(ctx.id, cx).expect("the tab has a toolbar");
        let popup = toolbar.read(cx).popup.clone().expect("the popup is open");
        popup.read(cx).recent().read(cx).on_open().clone()
    });
    let picked = PathBuf::from(second.path());
    cx.update_window(handle, |_, window, cx| on_open(picked, window, cx))
        .expect("window");
    for _ in 0..20 {
        cx.run_until_parked();
    }
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::OPEN_POPUP).is_none(),
            "choosing a repository closes the popup"
        );
    });
    cx.update(|cx| {
        let view = app.read(cx);
        assert_eq!(view.tabs().len(), 1);
        let path = view.tabs()[0].path(cx).expect("path");
        assert_eq!(path.canonicalize().ok(), second.path().canonicalize().ok());
    });
}

#[gpui_kit::test]
fn popup_recent_list_leaves_out_the_current_repository_and_cannot_remove(cx: &mut TestAppContext) {
    let first = Fixture::new();
    first.write("a.txt", "one\n");
    first.commit_all("first");
    let second = Fixture::new();
    second.write("b.txt", "two\n");
    second.commit_all("second");
    let (handle, _app, tab) = mount_fixture(cx, &first, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    let (first_dir, second_dir) = (PathBuf::from(first.path()), PathBuf::from(second.path()));
    cx.update(|cx| {
        crate::settings::state::update(cx, |s| {
            s.push_recent(&second_dir);
            s.push_recent(&first_dir);
        })
    });
    frame(cx, handle, |window, cx| open_popup::open(&ctx, window, cx));
    frame(cx, handle, |window, cx| {
        let toolbar = toolbar_of(ctx.id, cx).expect("the tab has a toolbar");
        let popup = toolbar.read(cx).popup.clone().expect("the popup is open");
        let recent = popup.read(cx).recent().clone();
        assert_eq!(
            recent.read(cx).shown(),
            std::slice::from_ref(&second_dir),
            "the current repository is not offered (07 section 5.2)"
        );
        assert!(
            recent.read(cx).search_focused(window, cx),
            "the search field has the focus when the popup opens"
        );
        assert!(
            window
                .try_find((crate::shell::ids::RECENT_ROW, 0usize))
                .is_some(),
            "the row is drawn"
        );
        assert!(
            window
                .try_find((crate::shell::ids::RECENT_REMOVE, 0usize))
                .is_none(),
            "rows of the popup have no remove button"
        );
    });
}

#[gpui_kit::test]
fn undo_slot_and_terminal_state_rendered(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| {
        // Milestone 1: Undo, Redo and Terminal are disabled; clicking them runs nothing.
        window.click(crate::undo::ids::TOOLBAR_UNDO, cx);
        window.click(crate::undo::ids::TOOLBAR_REDO, cx);
        window.click(ids::TERMINAL, cx);
    });
    assert!(ops(&tab, cx).is_empty());
    let ctx = ctx_of(&tab, cx);
    cx.update(|cx| {
        let state = crate::terminal::toolbar_button_state(&ctx, cx);
        assert!(!state.enabled);
        assert_eq!(state.tooltip, crate::terminal::M1_TOOLTIP);
        assert!(state.status_dot.is_none());
    });
}

#[gpui_kit::test]
fn terminal_button_draws_the_status_dot_at_the_icons_top_right(cx: &mut TestAppContext) {
    init_visual(cx);
    let (handle, _) = open_probe(cx, 400., 100., |_, cx| {
        let ctx = inert_tab_ctx(cx);
        terminal_button(
            ctx,
            TerminalButtonState {
                enabled: true,
                tooltip: "Show terminal".into(),
                status_dot: Some(hsla(0.3, 1., 0.5, 1.)),
            },
        )
        .into_any_element()
    });
    frame(cx, handle, |window, _| {
        let button = bounds(window, ids::TERMINAL);
        let dot = bounds(window, ids::TERMINAL_DOT);
        assert!(((dot.origin.x - button.origin.x).as_f32() - 36.).abs() <= 0.5);
        assert!(((dot.size.width).as_f32() - 8.).abs() <= 0.5);
        assert!(dot.origin.y - button.origin.y < gpui_kit::px(16.));
    });
}

#[gpui_kit::test]
fn branch_and_settings_open_their_dialogs(cx: &mut TestAppContext) {
    let (handle, app, _tab) = mount_seeded(cx, with_changes(), DEFAULT_SIZE);
    frame(cx, handle, |window, cx| window.click(ids::BRANCH, cx));
    frame(cx, handle, |window, cx| window.click(ids::SETTINGS, cx));
    // Until the dialogs of G10/G11 exist their entry points answer with an Info toast.
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("New branch")) || cx.update(|cx| has_dialog(handle, cx)),
        "Branch opens the New branch dialog: {toasts:?}"
    );
    assert!(
        toasts.iter().any(|t| t.contains("Settings")) || cx.update(|cx| has_dialog(handle, cx)),
        "Settings opens the Settings dialog: {toasts:?}"
    );
}

fn has_dialog(handle: AnyWindowHandle, cx: &mut App) -> bool {
    cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap_or(false)
}

#[test]
fn tooltips_follow_the_spec() {
    let all = ToolbarState {
        has_remote: true,
        can_stash: true,
        has_stash: true,
        pull_with_rebase: false,
    };
    assert_eq!(all.pull_tooltip(), "Pull current branch");
    assert_eq!(all.push_tooltip(), "Push current branch changes");
    assert_eq!(all.stash_tooltip(), "Stash uncommitted changes");
    assert_eq!(all.pop_tooltip(), "Pop the last stash");
}

#[test]
fn hints_use_gitnuros_order() {
    if cfg!(target_os = "macos") {
        return;
    }
    assert_eq!(state::primary_hint(false, false, "O"), "Ctrl+O");
    assert_eq!(state::primary_hint(false, true, "S"), "Ctrl+Shift+S");
    assert_eq!(state::primary_hint(true, false, "S"), "Ctrl+Alt+S");
}
