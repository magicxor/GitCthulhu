use core::prelude::v1::test;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};

use super::plan::{self, action_label};
use super::{ids, open};
use crate::backend::present::OpFailure;
use crate::backend::{
    GitError, ObjectId, OpOutcome, RebaseAction, RebaseOutcome, RebasePlan, RebaseStep,
};
use crate::tab::ops::OpKind;
use crate::tab::ui::{RepoUiEvent, RightContent, Selection};
use crate::tab::{RepoTab, RepoTabEvent};
use crate::test_support::seed::{full, oid};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, geometry::assert_rect, geometry::bounds, mount_fixture,
    mount_seeded, script_op, toast_texts,
};

thread_local! {
    static STARTED: RefCell<Vec<RebasePlan>> = const { RefCell::new(Vec::new()) };
}

/// Records the plan "Complete rebase" handed to the operation (the operation itself belongs to G4's catalogue).
pub(super) fn record_start(plan: &RebasePlan) {
    STARTED.with(|s| s.borrow_mut().push(plan.clone()));
}

fn started() -> Vec<RebasePlan> {
    STARTED.with(|s| s.borrow().clone())
}

fn step(n: u64, action: RebaseAction) -> RebaseStep {
    let summary = format!("Commit {n}");
    RebaseStep {
        action,
        commit: oid(n),
        message: format!("{summary}\n\nBody of commit {n}\n"),
        summary,
        new_message: None,
    }
}

fn plan_of(actions: &[RebaseAction]) -> RebasePlan {
    RebasePlan {
        head: oid(100),
        head_ref: Some(full("refs/heads/main")),
        base: Some(oid(99)),
        steps: actions
            .iter()
            .enumerate()
            .map(|(i, a)| step(i as u64 + 1, *a))
            .collect(),
        merges_dropped: 0,
    }
}

fn picks(n: usize) -> RebasePlan {
    plan_of(&vec![RebaseAction::Pick; n])
}

fn commits(plan: &RebasePlan) -> Vec<ObjectId> {
    plan.steps.iter().map(|s| s.commit).collect()
}

// ---------------------------------------------------------------------------------------------- pure plan rules

#[test]
fn first_line_actions_limited() {
    let mut plan = picks(3);
    let first = plan::allowed_actions(&plan, 0);
    assert_eq!(
        first.iter().map(|a| action_label(*a)).collect::<Vec<_>>(),
        ["Pick", "Reword", "Drop"],
        "08 §6: the first line offers Pick, Reword, Drop"
    );
    let other = plan::allowed_actions(&plan, 1);
    assert_eq!(
        other.iter().map(|a| action_label(*a)).collect::<Vec<_>>(),
        ["Pick", "Reword", "Squash", "Fixup", "Edit", "Drop"]
    );
    // A dropped first line makes the second one the first kept line (Git rejects a leading squash).
    plan.steps[0].action = RebaseAction::Drop;
    assert_eq!(plan::allowed_actions(&plan, 1).len(), 3);
    assert_eq!(plan::allowed_actions(&plan, 2).len(), 6);
    let mut drafts = HashMap::new();
    assert!(
        !plan::set_action(&mut plan, 1, RebaseAction::Squash, &mut drafts),
        "squash on the first kept line is refused"
    );
}

#[test]
fn dropping_or_moving_keeps_the_first_kept_line_valid() {
    let mut drafts = HashMap::new();
    let mut plan = plan_of(&[
        RebaseAction::Pick,
        RebaseAction::Squash,
        RebaseAction::Fixup,
    ]);
    assert!(plan::set_action(
        &mut plan,
        0,
        RebaseAction::Drop,
        &mut drafts
    ));
    assert_eq!(
        plan.steps[1].action,
        RebaseAction::Pick,
        "the squash that would lead is turned into a pick"
    );
    assert_eq!(plan.steps[2].action, RebaseAction::Fixup);

    let mut plan = plan_of(&[RebaseAction::Pick, RebaseAction::Pick, RebaseAction::Squash]);
    assert!(plan::move_step(&mut plan, 2, 0));
    assert_eq!(plan.steps[0].action, RebaseAction::Pick);
    assert_eq!(commits(&plan), [oid(3), oid(1), oid(2)]);
    assert!(!plan::move_step(&mut plan, 1, 1));
    assert!(!plan::move_step(&mut plan, 0, 3));
}

#[test]
fn reword_message_is_parked_when_the_action_changes() {
    let mut drafts = HashMap::new();
    let mut plan = picks(2);
    assert!(plan::set_action(
        &mut plan,
        1,
        RebaseAction::Reword,
        &mut drafts
    ));
    plan::set_message(&mut plan, 1, "A better message");
    assert_eq!(
        plan.steps[1].new_message.as_deref(),
        Some("A better message")
    );
    assert_eq!(plan::display_message(&plan.steps[1]), "A better message");

    assert!(plan::set_action(
        &mut plan,
        1,
        RebaseAction::Pick,
        &mut drafts
    ));
    assert_eq!(
        plan.steps[1].new_message, None,
        "a pick must not carry a message: Git would use it"
    );
    assert!(plan::display_message(&plan.steps[1]).starts_with("Commit 2"));

    assert!(plan::set_action(
        &mut plan,
        1,
        RebaseAction::Reword,
        &mut drafts
    ));
    assert_eq!(
        plan.steps[1].new_message.as_deref(),
        Some("A better message"),
        "choosing Reword again restores the edit"
    );

    // Text equal to the original is no edit; messages of other actions are not stored.
    plan::set_message(&mut plan, 1, "Commit 2\n\nBody of commit 2");
    assert_eq!(plan.steps[1].new_message, None);
    plan::set_message(&mut plan, 0, "ignored");
    assert_eq!(plan.steps[0].new_message, None);
}

#[test]
fn blocker_and_final_plan() {
    let mut drafts = HashMap::new();
    let empty = plan_of(&[]);
    assert_eq!(
        plan::blocker(&empty),
        Some("There are no commits to rebase")
    );

    let mut plan = picks(3);
    assert_eq!(plan::blocker(&plan), None);
    plan::set_action(&mut plan, 2, RebaseAction::Reword, &mut drafts);
    plan::set_message(&mut plan, 2, "  \n");
    assert_eq!(
        plan::blocker(&plan),
        Some("A reworded commit message is empty")
    );
    plan::set_message(&mut plan, 2, "New text");
    assert_eq!(plan::blocker(&plan), None);

    plan.steps[1].new_message = Some("stray".into());
    let final_plan = plan::final_plan(&plan);
    assert_eq!(final_plan.steps[1].new_message, None);
    assert_eq!(final_plan.steps[2].new_message.as_deref(), Some("New text"));
}

// ---------------------------------------------------------------------------------------------- views

fn show_plan(cx: &mut TestAppContext, plan: RebasePlan) -> (AnyWindowHandle, Entity<RepoTab>) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    set_plan(cx, &tab, Some(plan));
    render(cx, handle);
    (handle, tab)
}

fn set_plan(cx: &mut TestAppContext, tab: &Entity<RepoTab>, plan: Option<RebasePlan>) {
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.clone();
        ui.update(cx, |ui, cx| {
            ui.rebase_plan = plan;
            cx.emit(RepoUiEvent::CenterChanged);
            cx.notify();
        });
    });
    cx.run_until_parked();
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    for _ in 0..2 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("window");
    }
}

fn current_plan(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Option<RebasePlan> {
    cx.update(|cx| tab.read(cx).ctx().ui.read(cx).rebase_plan.clone())
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| window.click(id, cx))
        .expect("window");
    cx.run_until_parked();
    render(cx, handle);
}

fn click_row_padding(cx: &mut TestAppContext, handle: AnyWindowHandle, ix: usize) {
    // The row's top-left corner is padding: a click there hits the row itself, not a child.
    cx.update_window(handle, |_, window, cx| {
        window.click_at(
            (ids::ROW, ix),
            gpui_kit::point(gpui_kit::px(4.), gpui_kit::px(4.)),
            cx,
        )
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
}

#[gpui_kit::test]
fn layout_and_side_panel_hidden(cx: &mut TestAppContext) {
    let (handle, _tab) = show_plan(cx, picks(3));
    cx.update_window(handle, |_, window, _| {
        assert!(
            window
                .try_find(crate::side_panel::ids::SIDE_PANEL)
                .is_none(),
            "the side panel is hidden while a plan is edited (07 §5.3, 08 §6)"
        );
        let editor = bounds(window, ids::REBASE);
        assert!(
            editor.size.width.as_f32() > 400.,
            "the editor fills the centre"
        );
        let (x, y) = (editor.origin.x.as_f32(), editor.origin.y.as_f32());

        // Title: padding start 16, top 16, bottom 8 inside its box, a 20 sp line between.
        let title = bounds(window, ids::TITLE);
        assert!((title.origin.x.as_f32() - x).abs() <= 0.5, "{title:?}");
        assert!((title.origin.y.as_f32() - y).abs() <= 0.5, "{title:?}");
        let title_text_h = title.size.height.as_f32() - 16. - 8.;
        assert!(
            (20. ..=32.).contains(&title_text_h),
            "a 20 sp line between the paddings: {title:?}"
        );

        // 16 + one 20 sp line (Inter, 1.21) + 8: the list starts 48.2 below the editor's top.
        assert!(
            (title.size.height.as_f32() - 48.2).abs() <= 0.5,
            "title box 16 + 24.2 + 8: {title:?}"
        );

        // Rows: padding h16 v8, dropdown 180 wide, message field >= 40 high, 8 apart.
        let list_top = title.origin.y.as_f32() + title.size.height.as_f32();
        let row0 = bounds(window, (ids::ROW, 0usize));
        assert!((row0.origin.y.as_f32() - list_top).abs() <= 0.5, "{row0:?}");
        assert!(
            (row0.origin.y.as_f32() - (y + 48.2)).abs() <= 0.5,
            "{row0:?}"
        );
        assert!((row0.origin.x.as_f32() - x).abs() <= 0.5);
        let action = bounds(window, (ids::ACTION, 0usize));
        assert!(
            (action.size.width.as_f32() - 180.).abs() <= 0.5,
            "{action:?}"
        );
        assert!(
            (action.origin.x.as_f32() - (x + 16.)).abs() <= 0.5,
            "{action:?}"
        );
        assert!(
            (action.size.height.as_f32() - 40.).abs() <= 0.5,
            "the dropdown is 40 high (v8 padding, 24 dp chevron): {action:?}"
        );
        let message = bounds(window, (ids::MESSAGE, 0usize));
        assert!(
            (message.origin.x.as_f32() - (action.origin.x.as_f32() + 180. + 8.)).abs() <= 0.5,
            "message starts 8 after the dropdown: {message:?}"
        );
        assert!(message.size.height.as_f32() >= 40. - 0.5, "{message:?}");
        assert!(
            (row0.size.height.as_f32() - (message.size.height.as_f32() + 16.)).abs() <= 0.5,
            "row = field + 2 * 8 padding: {row0:?} {message:?}"
        );
        assert!(
            (message.origin.x.as_f32() + message.size.width.as_f32()
                - (x + editor.size.width.as_f32() - 16.))
                .abs()
                <= 0.5,
            "the field ends 16 before the right edge: {message:?}"
        );
        let row1 = bounds(window, (ids::ROW, 1usize));
        assert!(
            (row1.origin.y.as_f32() - (row0.origin.y.as_f32() + row0.size.height.as_f32())).abs()
                <= 0.5,
            "rows are stacked without gaps"
        );

        // Footer: Complete ends 16 from the right and bottom, Cancel 8 before it.
        let complete = bounds(window, ids::COMPLETE);
        let cancel = bounds(window, ids::CANCEL);
        let right = x + editor.size.width.as_f32();
        let bottom = y + editor.size.height.as_f32();
        assert!(
            (complete.origin.x.as_f32() + complete.size.width.as_f32() - (right - 16.)).abs()
                <= 0.5
        );
        assert!(
            (complete.origin.y.as_f32() + complete.size.height.as_f32() - (bottom - 16.)).abs()
                <= 0.5
        );
        assert!(
            (complete.origin.x.as_f32()
                - (cancel.origin.x.as_f32() + cancel.size.width.as_f32() + 8.))
                .abs()
                <= 0.5,
            "Cancel ends 8 before Complete: {cancel:?} {complete:?}"
        );
        assert_rect(
            window,
            ids::REBASE,
            editor.origin.x.as_f32(),
            editor.origin.y.as_f32(),
            editor.size.width.as_f32(),
            editor.size.height.as_f32(),
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn single_line_rows_are_56_high(cx: &mut TestAppContext) {
    let mut plan = picks(2);
    for s in &mut plan.steps {
        s.message = format!("{}\n", s.summary);
    }
    let (handle, _tab) = show_plan(cx, plan);
    cx.update_window(handle, |_, window, _| {
        let message = bounds(window, (ids::MESSAGE, 0usize));
        assert!(
            (message.size.height.as_f32() - 40.).abs() <= 0.5,
            "a one-line message keeps the 40 dp minimum: {message:?}"
        );
        let row = bounds(window, (ids::ROW, 0usize));
        assert!(
            (row.size.height.as_f32() - 56.).abs() <= 0.5,
            "8 + 40 + 8: {row:?}"
        );
        let action = bounds(window, (ids::ACTION, 0usize));
        assert!(
            (action.origin.y.as_f32() - (row.origin.y.as_f32() + 8.)).abs() <= 0.5,
            "the dropdown is centred in the row: {action:?} {row:?}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn dropdown_offers_the_limited_set_on_the_first_line(cx: &mut TestAppContext) {
    let (handle, _tab) = show_plan(cx, picks(3));
    click(cx, handle, (ids::ACTION, 0usize));
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find("menu-item-rebase-action-pick").is_some());
        assert!(window.try_find("menu-item-rebase-action-reword").is_some());
        assert!(window.try_find("menu-item-rebase-action-drop").is_some());
        assert!(window.try_find("menu-item-rebase-action-squash").is_none());
        assert!(window.try_find("menu-item-rebase-action-fixup").is_none());
        assert!(window.try_find("menu-item-rebase-action-edit").is_none());
    })
    .expect("window");
}

#[gpui_kit::test]
fn dropdown_choice_changes_the_action(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    click(cx, handle, (ids::ACTION, 2usize));
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find("menu-item-rebase-action-squash").is_some());
    })
    .expect("window");
    click(cx, handle, "menu-item-rebase-action-squash");
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(plan.steps[2].action, RebaseAction::Squash);
    assert_eq!(plan.steps[0].action, RebaseAction::Pick);
}

#[gpui_kit::test]
fn reword_enables_message_field(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    // A pick row's message is not editable: a click on it only selects the line.
    click(cx, handle, (ids::MESSAGE, 1usize));
    cx.update_window(handle, |_, window, cx| {
        // No text area exists in the frame yet: only static text fields.
        window.input("zzz", cx);
    })
    .expect("window");
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(plan.steps[1].new_message, None);
    assert_eq!(plan.steps[1].action, RebaseAction::Pick);

    // Make the line a Reword through the dropdown, then edit its message.
    click(cx, handle, (ids::ACTION, 1usize));
    click(cx, handle, "menu-item-rebase-action-reword");
    click(cx, handle, (ids::MESSAGE, 1usize));
    cx.update_window(handle, |_, window, cx| window.input("X", cx))
        .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(plan.steps[1].action, RebaseAction::Reword);
    let edited = plan.steps[1]
        .new_message
        .clone()
        .expect("typing stores the edited message");
    assert!(
        edited.contains('X'),
        "the typed text is part of the message: {edited:?}"
    );
    assert_eq!(edited.len(), "Commit 2\n\nBody of commit 2".len() + 1);
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.read(cx);
        assert_eq!(
            ui.selection.commit().map(|c| c.0),
            Some(oid(2)),
            "focusing the field selects the line (08 §6)"
        );
    });
}

#[gpui_kit::test]
fn row_click_selects_the_commit_for_the_right_pane(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    cx.update(|cx| {
        assert_eq!(tab.read(cx).ctx().ui.read(cx).right(), RightContent::Empty);
    });
    click_row_padding(cx, handle, 1);
    cx.update(|cx| {
        let ui = tab.read(cx).ctx().ui.read(cx);
        assert_eq!(
            ui.selection,
            Selection::Commit {
                id: oid(2),
                is_stash: false,
                scroll_to: false
            }
        );
        assert_eq!(
            ui.right(),
            RightContent::CommitChanges {
                commit: oid(2),
                stash: false
            },
            "the right pane shows CommitChanges of the selected line"
        );
    });
}

#[gpui_kit::test]
fn drag_reorders_steps(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    cx.update_window(handle, |_, window, cx| {
        window.drag_to((ids::ROW, 0usize), (ids::ROW, 2usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(
        commits(&plan),
        [oid(2), oid(3), oid(1)],
        "dragging the first row to the last slot moves it there"
    );
    assert!(
        plan.steps.iter().all(|s| s.action == RebaseAction::Pick),
        "reordering keeps the actions"
    );
}

#[gpui_kit::test]
fn keyboard_reorder_alt_up_down(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    click_row_padding(cx, handle, 1);
    cx.update_window(handle, |_, window, cx| window.press("alt-up", cx))
        .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(
        commits(&plan),
        [oid(2), oid(1), oid(3)],
        "alt-up moves the selected line up"
    );

    cx.update_window(handle, |_, window, cx| window.press("alt-down", cx))
        .expect("window");
    cx.update_window(handle, |_, window, cx| window.press("alt-down", cx))
        .expect("window");
    cx.run_until_parked();
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(
        commits(&plan),
        [oid(1), oid(3), oid(2)],
        "alt-down moves it down; at the bottom it stops"
    );
    cx.update_window(handle, |_, window, cx| window.press("alt-down", cx))
        .expect("window");
    let plan = current_plan(cx, &tab).expect("plan");
    assert_eq!(commits(&plan), [oid(1), oid(3), oid(2)]);
}

#[gpui_kit::test]
fn cancel_discards_plan(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(3));
    click(cx, handle, ids::CANCEL);
    assert_eq!(current_plan(cx, &tab), None);
    cx.update(|cx| assert!(tab.read(cx).views.rebase.is_none(), "the view is dropped"));
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::REBASE).is_none());
        assert!(
            window
                .try_find(crate::side_panel::ids::SIDE_PANEL)
                .is_some(),
            "the side panel is back"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn complete_starts_op_with_plan(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(
        cx,
        plan_of(&[RebaseAction::Pick, RebaseAction::Squash, RebaseAction::Pick]),
    );
    let before = started().len();
    // The operation fails to start, so the plan stays for a retry.
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Err::<OpOutcome<RebaseOutcome>, _>(OpFailure::Git(Arc::new(GitError::Unsupported {
                what: "a scripted failure",
            }))),
            Duration::ZERO,
        )
    });
    // Reorder first: the plan handed over is the edited one.
    click_row_padding(cx, handle, 2);
    cx.update_window(handle, |_, window, cx| window.press("alt-up", cx))
        .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    click(cx, handle, ids::COMPLETE);
    let all = started();
    assert_eq!(
        all.len(),
        before + 1,
        "Complete rebase starts exactly one operation"
    );
    let plan = all.last().expect("a started plan");
    assert_eq!(commits(plan), [oid(1), oid(3), oid(2)]);
    assert_eq!(plan.head, oid(100));
    assert_eq!(plan.base, Some(oid(99)));
    assert_eq!(plan.steps[2].action, RebaseAction::Squash);
    assert!(
        current_plan(cx, &tab).is_some(),
        "the plan stays until the operation succeeds"
    );
}

#[gpui_kit::test]
fn complete_is_disabled_without_commits_or_with_an_empty_reword(cx: &mut TestAppContext) {
    let (handle, _tab) = show_plan(cx, plan_of(&[]));
    let before = started().len();
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::EMPTY).is_some(),
            "an empty plan says so"
        );
    })
    .expect("window");
    click(cx, handle, ids::COMPLETE);
    assert_eq!(started().len(), before, "a disabled button starts nothing");
}

#[gpui_kit::test]
fn complete_is_disabled_with_an_empty_reword(cx: &mut TestAppContext) {
    // A Reword whose message was emptied blocks the start (Git would abort on it).
    let mut plan = plan_of(&[RebaseAction::Pick, RebaseAction::Reword]);
    plan.steps[1].new_message = Some("  \n".into());
    let (handle, tab) = show_plan(cx, plan);
    let before = started().len();
    click(cx, handle, ids::COMPLETE);
    assert_eq!(started().len(), before, "an empty reword starts nothing");
    assert!(current_plan(cx, &tab).is_some());
}

#[gpui_kit::test]
fn selecting_text_in_the_reword_field_does_not_drag_the_row(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(
        cx,
        plan_of(&[RebaseAction::Pick, RebaseAction::Reword, RebaseAction::Pick]),
    );
    click(cx, handle, (ids::MESSAGE, 1usize));
    cx.update_window(handle, |_, window, cx| {
        window.drag_to((ids::MESSAGE, 1usize), (ids::ROW, 2usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    assert_eq!(
        commits(&current_plan(cx, &tab).expect("plan")),
        [oid(1), oid(2), oid(3)],
        "a drag inside the text area selects text, it does not reorder"
    );
    // The row itself still drags.
    cx.update_window(handle, |_, window, cx| {
        window.drag_to((ids::ROW, 0usize), (ids::ROW, 2usize), cx)
    })
    .expect("window");
    cx.run_until_parked();
    render(cx, handle);
    assert_eq!(
        commits(&current_plan(cx, &tab).expect("plan")),
        [oid(2), oid(3), oid(1)]
    );
}

#[gpui_kit::test]
fn finished_operation_clears_the_plan_but_a_failed_one_keeps_it(cx: &mut TestAppContext) {
    let (handle, tab) = show_plan(cx, picks(2));
    let events = cx.update(|cx| tab.read(cx).ctx().events.clone());
    cx.update(|cx| {
        events.update(cx, |_, cx| {
            cx.emit(RepoTabEvent::OpFinished {
                kind: OpKind::RebaseInteractive,
                ok: false,
            })
        })
    });
    cx.run_until_parked();
    assert!(
        current_plan(cx, &tab).is_some(),
        "a failed start keeps the plan"
    );
    cx.update(|cx| {
        events.update(cx, |_, cx| {
            cx.emit(RepoTabEvent::OpFinished {
                kind: OpKind::Fetch,
                ok: true,
            })
        })
    });
    cx.run_until_parked();
    assert!(
        current_plan(cx, &tab).is_some(),
        "other operations do not matter"
    );
    cx.update(|cx| {
        events.update(cx, |_, cx| {
            cx.emit(RepoTabEvent::OpFinished {
                kind: OpKind::RebaseInteractive,
                ok: true,
            })
        })
    });
    cx.run_until_parked();
    render(cx, handle);
    assert_eq!(
        current_plan(cx, &tab),
        None,
        "the rebase started: the plan is spent"
    );
}

#[gpui_kit::test]
fn merge_note_and_stale_plan_replacement(cx: &mut TestAppContext) {
    let mut plan = picks(2);
    plan.merges_dropped = 2;
    let (handle, tab) = show_plan(cx, plan);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::MERGES_NOTE).is_some());
        assert!(window.try_find((ids::ROW, 1usize)).is_some());
        assert!(window.try_find((ids::ROW, 2usize)).is_none());
    })
    .expect("window");
    // Another plan replaces the first one while the editor is open.
    set_plan(cx, &tab, Some(picks(3)));
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::MERGES_NOTE).is_none());
        assert!(
            window.try_find((ids::ROW, 2usize)).is_some(),
            "the list follows the new plan"
        );
    })
    .expect("window");
}

// ---------------------------------------------------------------------------------------------- real repository

fn fixture_with_commits(n: usize) -> Fixture {
    let fixture = Fixture::new();
    for i in 1..=n {
        fixture.write("file.txt", &format!("version {i}\n"));
        fixture.commit_all(&format!("Subject {i}\n\nBody {i}"));
    }
    fixture
}

fn rev(fixture: &Fixture, spec: &str) -> ObjectId {
    ObjectId::from_hex(fixture.git(&["rev-parse", spec]).trim().as_bytes()).expect("an object id")
}

#[gpui_kit::test]
fn open_reads_the_plan_of_a_real_repository(cx: &mut TestAppContext) {
    let fixture = fixture_with_commits(4);
    let (handle, app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let base = rev(&fixture, "HEAD~2");
    cx.update(|cx| open(&ctx, Some(base), cx));
    cx.run_until_parked();
    render(cx, handle);
    let plan = current_plan(cx, &tab).expect("the plan was loaded");
    assert_eq!(plan.base, Some(base));
    assert_eq!(plan.head, rev(&fixture, "HEAD"));
    assert_eq!(
        plan.steps
            .iter()
            .map(|s| s.summary.as_str())
            .collect::<Vec<_>>(),
        ["Subject 3", "Subject 4"],
        "oldest first, the base itself excluded"
    );
    assert!(plan.steps.iter().all(|s| s.action == RebaseAction::Pick));
    assert!(
        plan.steps[0].message.contains("Body 3"),
        "the full message is kept"
    );
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::REBASE).is_some(), "the editor shows");
        assert!(
            window
                .try_find(crate::side_panel::ids::SIDE_PANEL)
                .is_none()
        );
        assert!(window.try_find((ids::ROW, 1usize)).is_some());
    })
    .expect("window");

    // From the root: every commit. Of two requests in a row only the newer one applies.
    cx.update(|cx| {
        open(&ctx, Some(rev(&fixture, "HEAD~1")), cx);
        open(&ctx, None, cx);
    });
    cx.run_until_parked();
    assert_eq!(current_plan(cx, &tab).expect("plan").steps.len(), 4);
    assert_eq!(current_plan(cx, &tab).expect("plan").base, None);

    // Nothing above the base: a toast, the open plan stays as it was.
    set_plan(cx, &tab, None);
    let head = rev(&fixture, "HEAD");
    cx.update(|cx| open(&ctx, Some(head), cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(current_plan(cx, &tab), None);
    cx.update(|cx| {
        assert!(
            toast_texts(&app, cx)
                .iter()
                .any(|t| t == "There are no commits to rebase"),
            "{:?}",
            toast_texts(&app, cx)
        );
    });
}

#[gpui_kit::test]
fn open_reports_an_invalid_base(cx: &mut TestAppContext) {
    let fixture = fixture_with_commits(2);
    let (_handle, app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| open(&ctx, Some(oid(12345)), cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(current_plan(cx, &tab), None);
    cx.update(|cx| {
        assert!(
            toast_texts(&app, cx)
                .iter()
                .any(|t| t.starts_with("Planning the interactive rebase failed")),
            "{:?}",
            toast_texts(&app, cx)
        );
    });
}
