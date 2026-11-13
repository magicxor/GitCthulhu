use core::prelude::v1::test;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, Entity, TestAppContext};

use super::*;
use crate::ids as app_ids;
use crate::tab::RepoTab;
use crate::test_support::geometry::{assert_height, assert_rect, assert_width, bounds};
use crate::test_support::{DEFAULT_SIZE, RepoSeed, TestCall, calls, mount_seeded, toast_texts};
use crate::toolbar::platform;

struct Open {
    handle: AnyWindowHandle,
    app: Entity<crate::app_view::AppView>,
    tab: Entity<RepoTab>,
}

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

/// A seeded tab with the dialog open and the search field focused.
fn open_dialog(cx: &mut TestAppContext) -> Open {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| platform::install_fake(cx, None));
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    frame(cx, handle, |window, cx| open(window, cx, ctx));
    for _ in 0..3 {
        frame(cx, handle, |_, _| ());
    }
    Open { handle, app, tab }
}

fn reloaded_all(open: &Open, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        calls(&open.tab, cx)
            .iter()
            .any(|c| matches!(c, TestCall::Reload(set) if *set == RefreshSet::ALL))
    })
}

fn has_dialog(open: &Open, cx: &mut TestAppContext) -> bool {
    cx.update_window(open.handle, |_, window, cx| window.has_active_dialog(cx))
        .expect("window")
}

fn file_manager_calls(cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| platform::calls(cx))
        .into_iter()
        .filter(|call| call.starts_with("open:"))
        .collect()
}

#[gpui_kit::test]
fn quick_actions_layout(cx: &mut TestAppContext) {
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, _| {
        assert_width(window, app_ids::DIALOG_FRAME, 680. + 32.);
        assert_width(window, ids::QUICK_ACTIONS, 680.);
        assert_height(window, ids::QUICK_ACTIONS, 400.);
        assert_width(window, ids::FIELD, 680.);
        let field = bounds(window, ids::FIELD);
        let first = bounds(window, (ids::ROW, 0usize));
        assert!(
            ((first.top() - field.bottom()).as_f32() - 16.).abs() <= 0.5,
            "16 dp between the field and the list"
        );
        for ix in 0..4usize {
            assert_height(window, (ids::ROW, ix), 56.);
            assert_width(window, (ids::ROW, ix), 680.);
            // Icon 24 with 16 padding on every side: a 56 x 56 box at the row's start (09 §3.13).
            let row = bounds(window, (ids::ROW, ix));
            assert_rect(
                window,
                (ids::ROW_ICON, ix),
                row.origin.x.as_f32(),
                row.origin.y.as_f32(),
                56.,
                56.,
            );
        }
        assert_rect(
            window,
            (ids::ROW, 1usize),
            first.origin.x.as_f32(),
            first.origin.y.as_f32() + 56.,
            680.,
            56.,
        );
        assert!(
            window.try_find((ids::ROW, 4usize)).is_none(),
            "four actions"
        );
    });
}

#[gpui_kit::test]
fn quick_actions_keyboard(cx: &mut TestAppContext) {
    // Down and Up move the selection and clamp at both ends; Enter runs the selected row.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        for _ in 0..4 {
            window.press("down", cx);
        }
        for _ in 0..5 {
            window.press("up", cx);
        }
        window.press("down", cx);
        window.press("down", cx);
        window.press("enter", cx);
    });
    assert!(
        reloaded_all(&open, cx),
        "after clamping, two Downs select \"Refresh repository data\" and Enter runs it"
    );
    assert!(
        !has_dialog(&open, cx),
        "running an action closes the dialog"
    );

    // Enter without moving runs the first action: the file manager at the repository path.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| window.press("enter", cx));
    let opened = file_manager_calls(cx);
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(
        opened[0] == "open:repo",
        "the repository's directory was opened: {opened:?}"
    );

    // Escape closes the dialog without running anything.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| window.press("escape", cx));
    assert!(!has_dialog(&open, cx), "Escape closes");
    assert!(!reloaded_all(&open, cx));
    assert!(file_manager_calls(cx).is_empty());
}

#[gpui_kit::test]
fn quick_actions_filter_resets_selection_and_runs_the_match(cx: &mut TestAppContext) {
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.input("REFRESH", cx);
    });
    frame(cx, open.handle, |window, _| {
        assert!(window.try_find((ids::ROW, 0usize)).is_some());
        assert!(
            window.try_find((ids::ROW, 1usize)).is_none(),
            "case-insensitive contains leaves one row"
        );
    });
    frame(cx, open.handle, |window, cx| window.press("enter", cx));
    assert!(reloaded_all(&open, cx), "the single match runs");

    // Typing resets the selection to the first match.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.press("down", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.input("o", cx);
    });
    frame(cx, open.handle, |window, cx| window.press("enter", cx));
    assert_eq!(
        file_manager_calls(cx).len(),
        1,
        "the first of the matches for \"o\" (the file manager) ran"
    );

    // No match: Enter does nothing and the dialog stays.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| window.input("zzz", cx));
    frame(cx, open.handle, |window, cx| window.press("enter", cx));
    assert!(has_dialog(&open, cx));
    assert!(!reloaded_all(&open, cx));
}

#[gpui_kit::test]
fn quick_actions_click_runs_and_hover_does_not_select(cx: &mut TestAppContext) {
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.hover((ids::ROW, 3usize), cx);
        window.press("enter", cx);
    });
    assert_eq!(
        file_manager_calls(cx).len(),
        1,
        "hovering the last row did not move the selection"
    );

    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.click((ids::ROW, 2usize), cx)
    });
    assert!(reloaded_all(&open, cx), "a click runs that row");
    assert!(!has_dialog(&open, cx));
}

#[gpui_kit::test]
fn quick_actions_replace_themselves_with_clone_and_signoff(cx: &mut TestAppContext) {
    // Until the dialogs of G10 exist their entry points answer with a toast; afterwards a dialog stands.
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.click((ids::ROW, 1usize), cx)
    });
    let toasts = cx.update(|cx| toast_texts(&open.app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("Clone")) || has_dialog(&open, cx),
        "Clone new repository opens the Clone dialog: {toasts:?}"
    );
    let open = open_dialog(cx);
    frame(cx, open.handle, |window, cx| {
        window.click((ids::ROW, 3usize), cx)
    });
    let toasts = cx.update(|cx| toast_texts(&open.app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("Sign-off")) || has_dialog(&open, cx),
        "Signoff config opens the Sign-off dialog: {toasts:?}"
    );
}

#[test]
fn matching_is_case_insensitive_contains_in_display_order() {
    assert_eq!(matching(""), [0, 1, 2, 3]);
    assert_eq!(matching("REPOSITORY"), [0, 1, 2]);
    assert_eq!(matching("sign"), [3]);
    assert!(matching("zzz").is_empty());
}
