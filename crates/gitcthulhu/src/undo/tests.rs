use core::prelude::v1::test;

use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, ElementId, Entity, TestAppContext, Window};

use super::texts::{self, ButtonTip, Which};
use super::*;
use crate::app_view::AppView;
use crate::backend::{
    ChangeKind, CommitOptions, EntryId, ObjectId, OperationKind, OplogInProgress as InProgress,
    PageRequest, Refusal, StackItemInfo, StartPoint, UndoOutcome, UndoRedoState,
};
use crate::dialogs::choice::ids as choice_ids;
use crate::tab::RepoTab;
use crate::tab::ops::RestoreDone;
use crate::test_support::geometry::{assert_rect, bounds};
use crate::test_support::seed::full;
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, mount_fixture_with, mount_seeded, script_op,
    toast_texts,
};

const BUTTON_Y: f32 = metrics::TAB_STRIP_H + metrics::TOOLBAR_PAD_V;

fn item(title: &str, ends: Option<InProgress>) -> StackItemInfo {
    StackItemInfo {
        reverses: EntryId(7),
        operation: EntryId(7),
        kind: OperationKind::Commit,
        title: title.to_owned(),
        finished_at_ms: 0,
        ends_in_progress: ends,
    }
}

fn state(
    undo: Option<StackItemInfo>,
    redo: Option<StackItemInfo>,
    interrupted: bool,
) -> UndoRedoState {
    UndoRedoState {
        undo_depth: usize::from(undo.is_some()),
        redo_depth: usize::from(redo.is_some()),
        undo,
        redo,
        interrupted_restore: interrupted,
    }
}

fn seeded_with(
    cx: &mut TestAppContext,
    undo: Option<UndoRedoState>,
) -> (AnyWindowHandle, Entity<AppView>, Entity<RepoTab>) {
    let mut seed = RepoSeed::linear(2).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    seed.undo = undo;
    mount_seeded(cx, seed, DEFAULT_SIZE)
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

fn controller(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<UndoController> {
    cx.update(|cx| tab.read(cx).views().undo.clone())
}

fn restores(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<RestoreKind> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|c| match c {
                TestCall::Restore(kind) => Some(kind),
                _ => None,
            })
            .collect()
    })
}

fn tips(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> (ButtonTip, ButtonTip) {
    let c = controller(tab, cx);
    cx.update(|cx| c.read(cx).tips(cx))
}

fn chevron() -> ElementId {
    ElementId::from((ElementId::from(ids::TOOLBAR_UNDO), "chevron"))
}

// ── pure texts ─────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn tooltips_texts() {
    let st = state(
        Some(item("Commit \"Fix login\"", None)),
        Some(item("Merge 'feature' into main", None)),
        false,
    );
    let undo = texts::button_tip(Which::Undo, Some(&st), None, false);
    assert!(undo.enabled);
    assert_eq!(undo.text, "Undo Commit \"Fix login\"");
    assert_eq!(undo.hint.as_deref(), Some("Ctrl+Z"));
    let redo = texts::button_tip(Which::Redo, Some(&st), None, false);
    assert_eq!(redo.text, "Redo Merge 'feature' into main");
    assert_eq!(redo.hint.as_deref(), Some("Ctrl+Shift+Z"));

    // An undo that also ends an operation in progress says so (17 §4.5).
    let st = state(
        Some(item("Merge 'feature' into main", Some(InProgress::Merge))),
        None,
        false,
    );
    let undo = texts::button_tip(Which::Undo, Some(&st), None, false);
    assert_eq!(
        undo.text,
        "Undo Merge 'feature' into main \u{2014} also ends the merge in progress (can't be redone)"
    );

    // Nothing to do.
    let st = state(None, None, false);
    for (which, text) in [
        (Which::Undo, "Nothing to undo"),
        (Which::Redo, "Nothing to redo"),
    ] {
        let tip = texts::button_tip(which, Some(&st), None, false);
        assert!(!tip.enabled);
        assert_eq!(tip.text, text);
        assert!(tip.hint.is_none(), "no hint while disabled");
    }

    // An interrupted undo disables both until it is finished or rolled back.
    let st = state(
        Some(item("Commit \"x\"", None)),
        Some(item("Commit \"y\"", None)),
        true,
    );
    for which in [Which::Undo, Which::Redo] {
        let tip = texts::button_tip(which, Some(&st), None, false);
        assert!(!tip.enabled);
        assert_eq!(tip.text, "Finish or roll back the interrupted undo first");
    }

    // Unavailable oplog: the reason.
    let reason: SharedString = "bare repository".into();
    let tip = texts::button_tip(Which::Undo, None, Some(&reason), false);
    assert!(!tip.enabled);
    assert_eq!(
        tip.text,
        "Undo is not available for this repository: bare repository"
    );
    // A text that already is the sentence is not prefixed twice.
    let full_text: SharedString =
        "Undo is not available for this repository: reftable ref storage".into();
    assert_eq!(texts::unavailable_text(&full_text), full_text);

    // A running operation disables the buttons but keeps the wording.
    let st = state(Some(item("Commit \"x\"", None)), None, false);
    let tip = texts::button_tip(Which::Undo, Some(&st), None, true);
    assert!(!tip.enabled);
    assert_eq!(tip.text, "Undo Commit \"x\"");
}

#[test]
fn relative_times() {
    let format = DateFormat::from_settings(&crate::settings::Settings::default());
    let now = 1_700_000_000_000;
    assert_eq!(texts::relative_time(now - 5_000, now, &format), "just now");
    assert_eq!(
        texts::relative_time(now - 5 * 60_000, now, &format),
        "5 min ago"
    );
    assert_eq!(
        texts::relative_time(now - 3 * 3_600_000, now, &format),
        "3 h ago"
    );
    assert!(
        !texts::relative_time(now - 40 * 3_600_000, now, &format).ends_with("ago"),
        "older entries show the date"
    );
}

use crate::widgets::date::DateFormat;
use gpui_kit::SharedString;

// ── toolbar buttons ────────────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn toolbar_geometry_of_the_undo_slot(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = seeded_with(cx, None);
    frame(cx, handle, |window, _| {
        let undo = bounds(window, ids::TOOLBAR_UNDO);
        assert_rect(
            window,
            ids::TOOLBAR_UNDO,
            f32::from(undo.origin.x),
            BUTTON_Y,
            64., // 21 §5.1: Undo 64 x 56
            56.,
        );
        let chev = bounds(window, chevron());
        assert!(
            (f32::from(chev.size.width) - 16.).abs() < 0.5,
            "16 dp chevron strip"
        );
        assert!(
            (f32::from(chev.origin.x - undo.origin.x) - 48.).abs() < 0.5,
            "the chevron strip is the right 16 dp of the button"
        );
        let redo = bounds(window, ids::TOOLBAR_REDO);
        assert!(
            (f32::from(redo.origin.x - undo.origin.x) - (64. + 4.)).abs() < 0.5,
            "Redo follows after the 4 dp gap"
        );
        assert_rect(
            window,
            ids::TOOLBAR_REDO,
            f32::from(redo.origin.x),
            BUTTON_Y,
            56., // 21 §5.1: Redo 56 x 56
            56.,
        );
    });
}

#[gpui_kit::test]
fn buttons_enabled_by_state(cx: &mut TestAppContext) {
    // Nothing recorded: both disabled, clicks do nothing.
    let (handle, _app, tab) = seeded_with(cx, Some(state(None, None, false)));
    frame(cx, handle, |window, cx| {
        window.click(ids::TOOLBAR_UNDO, cx);
        window.click(ids::TOOLBAR_REDO, cx);
    });
    assert!(restores(&tab, cx).is_empty());
    let (undo, redo) = tips(&tab, cx);
    assert!(!undo.enabled && !redo.enabled);

    // Only something to undo.
    let (handle, _app, tab) = seeded_with(
        cx,
        Some(state(Some(item("Commit \"x\"", None)), None, false)),
    );
    frame(cx, handle, |window, cx| {
        window.click(ids::TOOLBAR_REDO, cx);
    });
    assert!(restores(&tab, cx).is_empty(), "Redo is disabled");
    frame(cx, handle, |window, cx| {
        window.click(ids::TOOLBAR_UNDO, cx);
    });
    assert_eq!(restores(&tab, cx), [RestoreKind::Undo]);

    // Only something to redo.
    let (handle, _app, tab) = seeded_with(
        cx,
        Some(state(None, Some(item("Commit \"x\"", None)), false)),
    );
    frame(cx, handle, |window, cx| {
        window.click(ids::TOOLBAR_UNDO, cx);
        window.click(ids::TOOLBAR_REDO, cx);
    });
    assert_eq!(restores(&tab, cx), [RestoreKind::Redo]);

    // The oplog is unavailable: disabled, with the reason.
    let (handle, _app, tab) = seeded_with(cx, None);
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            m.undo_unavailable = Some("bare repository".into());
            cx.notify();
        });
    });
    frame(cx, handle, |window, cx| window.click(ids::TOOLBAR_UNDO, cx));
    assert!(restores(&tab, cx).is_empty());
    let (undo, _) = tips(&tab, cx);
    assert!(!undo.enabled);
    assert_eq!(
        undo.text,
        "Undo is not available for this repository: bare repository"
    );
}

#[gpui_kit::test]
fn buttons_are_disabled_while_an_operation_runs(cx: &mut TestAppContext) {
    let (handle, _app, tab) = seeded_with(
        cx,
        Some(state(Some(item("Commit \"x\"", None)), None, false)),
    );
    assert!(tips(&tab, cx).0.enabled);
    cx.update(|cx| {
        script_op::<crate::backend::OpOutcome<()>>(
            &tab,
            cx,
            Ok(crate::backend::OpOutcome {
                value: (),
                refresh: crate::backend::RefreshSet::empty(),
                log: Default::default(),
                warnings: Vec::new(),
            }),
            Duration::from_millis(500),
        )
    });
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| {
            t.run_op(
                crate::tab::ops::OpSpec::new(OpKind::Fetch, |_| {
                    Ok(crate::backend::OpOutcome {
                        value: (),
                        refresh: crate::backend::RefreshSet::empty(),
                        log: Default::default(),
                        warnings: Vec::new(),
                    })
                }),
                window,
                cx,
            )
        })
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        !tips(&tab, cx).0.enabled,
        "disabled while the operation runs"
    );
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert!(tips(&tab, cx).0.enabled, "enabled again afterwards");
}

#[gpui_kit::test]
fn refused_shows_error(cx: &mut TestAppContext) {
    for (kind, title) in [
        (RestoreKind::Undo, "Can't undo"),
        (RestoreKind::Redo, "Can't redo"),
    ] {
        let st = state(
            Some(item("Commit \"x\"", None)),
            Some(item("Commit \"y\"", None)),
            false,
        );
        let (handle, app, tab) = seeded_with(cx, Some(st));
        cx.update(|cx| {
            script_op::<RestoreDone>(
                &tab,
                cx,
                Ok(RestoreDone::Outcome(UndoOutcome::Refused(
                    Refusal::OperationInProgress {
                        kind: InProgress::Merge,
                    },
                ))),
                Duration::ZERO,
            )
        });
        cx.update_window(handle, |_, window, cx| {
            tab.update(cx, |t, cx| match kind {
                RestoreKind::Undo => t.undo(window, cx),
                _ => t.redo(window, cx),
            })
        })
        .expect("window");
        cx.run_until_parked();
        cx.run_until_parked();
        assert_eq!(
            cx.update(|cx| app.read(cx).errors_opened.clone()),
            vec![SharedString::from(title)],
            "the refusal opens an error dialog"
        );
    }
    assert_eq!(
        crate::backend::present::refusal_text(
            &Refusal::OperationInProgress {
                kind: InProgress::Merge
            },
            "Undo"
        ),
        "A merge is in progress. Finish or abort it first."
    );
    let in_the_way = Refusal::UntrackedInTheWay(vec![bstr::BString::from("new.txt")]);
    assert!(
        crate::backend::present::refusal_text(&in_the_way, "Redo")
            .starts_with("Redo needs to write"),
        "the refusal names the restore that was refused"
    );
}

// ── shortcuts ──────────────────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn shortcuts_outside_inputs_only(cx: &mut TestAppContext) {
    let st = state(
        Some(item("Commit \"x\"", None)),
        Some(item("Commit \"y\"", None)),
        false,
    );
    let (handle, _app, tab) = seeded_with(cx, Some(st));
    // A click inside the frame gives it the focus.
    frame(cx, handle, |window, cx| {
        window.click(crate::ids::FRAME_DIVIDER, cx)
    });
    // Focus in the frame: Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y act.
    for key in ["ctrl-z", "ctrl-shift-z", "ctrl-y"] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx))
            .expect("window");
        cx.run_until_parked();
    }
    assert_eq!(
        restores(&tab, cx),
        [RestoreKind::Undo, RestoreKind::Redo, RestoreKind::Redo]
    );
    // Focus in the commit message: the text input keeps its own undo.
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| {
        ctx.events.update(cx, |_, cx| {
            cx.emit(crate::tab::RepoTabEvent::FocusCommitMessage)
        })
    });
    cx.run_until_parked();
    frame(cx, handle, |_, _| {});
    for key in ["ctrl-z", "ctrl-shift-z", "ctrl-y"] {
        cx.update_window(handle, |_, window, cx| window.press(key, cx))
            .expect("window");
        cx.run_until_parked();
    }
    assert_eq!(
        restores(&tab, cx).len(),
        3,
        "no restore while a text input has the focus"
    );
}

// ── interrupted undo ───────────────────────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn interrupted_restore_recovery_dialog(cx: &mut TestAppContext) {
    let st = state(Some(item("Commit \"x\"", None)), None, true);
    let (handle, _app, tab) = seeded_with(cx, Some(st));
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, cx| {
        assert!(
            window.try_find(choice_ids::CHOICE_DIALOG).is_some(),
            "the Finish / Roll back question opens by itself"
        );
        window.click((choice_ids::CHOICE_OPTION, 1usize), cx);
    });
    cx.run_until_parked();
    assert!(
        cx.update(|cx| calls(&tab, cx)).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: OpKind::RecoverUndo,
                ..
            }
        )),
        "Roll back runs the recovery"
    );
    let (undo, redo) = tips(&tab, cx);
    assert_eq!(undo.text, "Finish or roll back the interrupted undo first");
    assert!(!undo.enabled && !redo.enabled);
    // The chevron asks again instead of opening the history.
    frame(cx, handle, |window, cx| window.click(chevron(), cx));
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::UNDO_HISTORY).is_none());
    });
}

/// A restore that ends interrupted already asks through the runner's recovery flow; the UNDO reload that follows must
/// not stack the same question again.
#[gpui_kit::test]
fn interrupted_restore_is_asked_once_when_the_runner_already_asks(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt as _;
    let st = state(Some(item("Commit \"x\"", None)), None, false);
    let (handle, _app, tab) = seeded_with(cx, Some(st));
    frame(cx, handle, |_, _| {});
    // The runner presents `RecoveryFlow::OplogInterrupted` (as `finish_restore` does for an interrupted restore).
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| {
            t.run_recovery(RecoveryFlow::OplogInterrupted, window, cx)
        })
    })
    .expect("window");
    cx.run_until_parked();
    // Then the reload reports the interruption.
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            if let Some(u) = m.undo.as_mut() {
                u.interrupted_restore = true;
            }
            cx.emit(RepoModelEvent(RefreshSet::UNDO));
            cx.notify();
        });
    });
    cx.run_until_parked();
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, cx| {
        assert!(window.has_active_dialog(cx));
        window.close_dialog(cx);
    });
    frame(cx, handle, |window, cx| {
        assert!(
            !window.has_active_dialog(cx),
            "only one Finish / Roll back question was open"
        );
    });
}

// ── history popover and toasts (real repository) ───────────────────────────────────────────────────────

fn opts(message: &str) -> CommitOptions {
    CommitOptions {
        message: message.into(),
        ..CommitOptions::default()
    }
}

fn run(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>),
) {
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| f(t, window, cx))
    })
    .expect("window");
    cx.run_until_parked();
    cx.run_until_parked();
}

fn head(fx: &Fixture) -> String {
    fx.git(&["rev-parse", "HEAD"]).trim().to_owned()
}

fn real_tab(
    cx: &mut TestAppContext,
    ask: bool,
) -> (Fixture, AnyWindowHandle, Entity<AppView>, Entity<RepoTab>) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("b.txt", "bee\n");
    fx.git(&["add", "b.txt"]);
    let (handle, app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, ask);
    (fx, handle, app, tab)
}

/// Clicks the action of the newest toast.
fn click_toast_action(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    let clicked = cx
        .update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let newest = (0usize..64)
                .rev()
                .find(|id| window.try_find(("toast-action", *id)).is_some());
            if let Some(id) = newest {
                window.click(("toast-action", id), cx);
            }
            newest.is_some()
        })
        .expect("window");
    cx.run_until_parked();
    cx.run_until_parked();
    clicked
}

fn branches(fx: &Fixture) -> String {
    fx.git(&["branch", "--format=%(refname:short)"])
}

#[gpui_kit::test]
fn success_toast_has_undo_action(cx: &mut TestAppContext) {
    let (fx, handle, app, tab) = real_tab(cx, true);
    let before = branches(&fx);
    run(cx, handle, &tab, |t, w, cx| {
        t.create_branch("topic".to_owned(), StartPoint::Head, false, w, cx);
    });
    let created = branches(&fx);
    assert_ne!(before, created);
    assert!(
        click_toast_action(cx, handle),
        "the success toast has an action"
    );
    assert_eq!(
        branches(&fx),
        before,
        "the toast's Undo undid the operation"
    );
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        texts.iter().any(|t| t == "Undone: Create branch 'topic'"),
        "{texts:?}"
    );
    // The undo toast offers Redo.
    assert!(
        click_toast_action(cx, handle),
        "the undo toast has an action"
    );
    assert_eq!(
        branches(&fx),
        created,
        "the toast's Redo redid the operation"
    );
}

#[gpui_kit::test]
fn toast_action_does_nothing_after_a_newer_operation(cx: &mut TestAppContext) {
    let (fx, handle, app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.commit(opts("Add b"), w, cx);
    });
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let action = toast_action(
        &ctx,
        ToastContext::Recorded {
            kind: OpKind::Commit,
        },
    )
    .expect("a recorded operation has an action");
    cx.run_until_parked();
    // Another recorded operation comes before the click.
    let tip = ObjectId::from_hex(head(&fx).as_bytes()).expect("a commit id");
    run(cx, handle, &tab, |t, w, cx| {
        t.create_tag("v1".to_owned(), tip, None, w, cx);
    });
    let after = head(&fx);
    let tags = fx.git(&["tag", "--list"]);
    cx.update_window(handle, |_, window, cx| (action.run)(window, cx))
        .expect("window");
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(head(&fx), after, "HEAD is untouched");
    assert_eq!(
        fx.git(&["tag", "--list"]),
        tags,
        "the newer operation is untouched"
    );
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert!(texts.iter().any(|t| t == STALE_TOAST), "{texts:?}");
}

#[gpui_kit::test]
fn toast_action_of_a_no_op_does_not_undo_an_older_operation(cx: &mut TestAppContext) {
    let (fx, handle, app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.create_branch("topic".to_owned(), StartPoint::Head, false, w, cx);
    });
    let with_topic = branches(&fx);
    // Checking out the current branch changes nothing: the oplog drops its entry, so its toast offers no "Undo".
    run(cx, handle, &tab, |t, w, cx| {
        t.checkout_branch(full("refs/heads/main"), w, cx);
    });
    assert_eq!(branches(&fx), with_topic);
    assert_eq!(
        undo_depth(&tab, cx),
        1,
        "only the branch creation is on the undo stack"
    );
    // The newest action left is the older toast's "Undo"; a newer operation ran since, so it does nothing.
    assert!(
        click_toast_action(cx, handle),
        "the branch toast still shows its action"
    );
    assert_eq!(
        branches(&fx),
        with_topic,
        "the older operation was not undone"
    );
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert!(texts.iter().any(|t| t == STALE_TOAST), "{texts:?}");
}

#[gpui_kit::test]
fn a_no_op_toast_has_no_undo_action(cx: &mut TestAppContext) {
    let (_fx, handle, app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.checkout_branch(full("refs/heads/main"), w, cx);
    });
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert_eq!(texts.len(), 1, "the checkout's success toast: {texts:?}");
    assert!(
        !click_toast_action(cx, handle),
        "an operation that changed nothing offers no Undo"
    );
    assert_eq!(undo_depth(&tab, cx), 0);
}

fn open_history(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: &Entity<RepoTab>) {
    let c = controller(tab, cx);
    cx.update_window(handle, |_, window, cx| {
        c.update(cx, |c, cx| c.open_history(window, cx))
    })
    .expect("window");
    cx.run_until_parked();
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |_, _| {});
}

#[gpui_kit::test]
fn history_popover_geometry_and_rows(cx: &mut TestAppContext) {
    let (_fx, handle, _app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.commit(opts("Add b"), w, cx);
    });
    // Opening through the chevron.
    frame(cx, handle, |window, cx| window.click(chevron(), cx));
    cx.run_until_parked();
    frame(cx, handle, |_, _| {});
    frame(cx, handle, |window, _| {
        let chev = bounds(window, chevron());
        assert_rect(
            window,
            ids::UNDO_HISTORY,
            f32::from(chev.origin.x),
            f32::from(chev.origin.y + chev.size.height),
            360., // 21 §5.1: 360 x 420 dp
            420.,
        );
        let row = bounds(window, (ids::UNDO_HISTORY_ROW, 0usize));
        assert!(
            (f32::from(row.size.height) - 36.).abs() < 0.5,
            "rows are 36 dp"
        );
        assert!(
            window.try_find((ids::UNDO_HISTORY_ROW, 1usize)).is_none(),
            "one entry only"
        );
        assert!(window.try_find(ids::UNDO_HISTORY_CLEAR).is_some());
        assert!(window.try_find(ids::UNDO_HISTORY_NOTE).is_some());
    });
    let c = controller(&tab, cx);
    let entries = cx.update(|cx| {
        c.read(cx).history().map(|p| {
            p.read(cx)
                .entries()
                .iter()
                .map(|e| (e.kind, e.title.clone()))
                .collect::<Vec<_>>()
        })
    });
    let entries = entries.expect("the popover is open");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, OperationKind::Commit);
    assert_eq!(entries[0].1, "Commit \"Add b\"");

    // Escape closes it.
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    cx.run_until_parked();
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::UNDO_HISTORY).is_none());
    });
}

#[gpui_kit::test]
fn history_popover_list_paging_and_restore(cx: &mut TestAppContext) {
    let (fx, handle, _app, tab) = real_tab(cx, true);
    let first = head(&fx);
    let tip = ObjectId::from_hex(first.as_bytes()).expect("a commit id");
    // Twelve recorded operations, read in pages of five.
    for n in 0..12 {
        run(cx, handle, &tab, |t, w, cx| {
            t.create_tag(format!("t{n}"), tip, None, w, cx);
        });
    }
    let c = controller(&tab, cx);
    c.update(cx, |c, _| c.set_page_size(5));
    open_history(cx, handle, &tab);
    let (len, more) = cx.update(|cx| {
        let p = c.read(cx).history().expect("open").read(cx);
        (p.entries().len(), p.has_more())
    });
    assert_eq!(len, 12, "all three pages were read as the list was shown");
    assert!(!more, "the last page ended the history");
    // Newest first.
    let titles: Vec<String> = cx.update(|cx| {
        c.read(cx)
            .history()
            .expect("open")
            .read(cx)
            .entries()
            .iter()
            .map(|e| e.title.clone())
            .collect()
    });
    assert_eq!(titles[0], "Create tag 't11'");
    assert_eq!(titles[11], "Create tag 't0'");

    // Right-click the third row: "Restore to before this operation" (t9 was created third from the end).
    let state_before = fx.git(&["tag", "--list"]);
    assert_eq!(state_before.lines().count(), 12);
    frame(cx, handle, |window, cx| {
        window.right_click((ids::UNDO_HISTORY_ROW, 2usize), cx);
    });
    frame(cx, handle, |window, cx| {
        assert!(window.try_find("menu-item-undo-restore-before").is_some());
        assert!(window.try_find("menu-item-undo-restore-after").is_some());
        window.click("menu-item-undo-restore-before", cx);
    });
    cx.run_until_parked();
    cx.run_until_parked();
    let tags = fx.git(&["tag", "--list"]);
    assert!(
        !tags.lines().any(|t| t == "t9") && tags.lines().any(|t| t == "t8"),
        "the repository is back to just before t9 was created: {tags}"
    );
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(ids::UNDO_HISTORY).is_none(),
            "the popover closed"
        );
    });
}

#[gpui_kit::test]
fn clear_history_from_the_popover_confirms(cx: &mut TestAppContext) {
    let (fx, handle, app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.commit(opts("Add b"), w, cx);
    });
    let committed = head(&fx);
    open_history(cx, handle, &tab);
    frame(cx, handle, |window, cx| {
        window.click(ids::UNDO_HISTORY_CLEAR, cx)
    });
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(head(&fx), committed, "the repository is untouched");
    let st = cx.update(|cx| tab.read(cx).ctx().model.read(cx).undo.clone());
    assert!(
        st.is_some_and(|s| s.undo.is_none() && s.redo.is_none()),
        "the history is empty"
    );
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        texts.iter().any(|t| t == "Undo history cleared"),
        "{texts:?}"
    );
    let (undo, _) = tips(&tab, cx);
    assert!(!undo.enabled);
    // Reading the list again shows the empty state.
    open_history(cx, handle, &tab);
    frame(cx, handle, |window, _| {
        assert!(window.try_find(ids::UNDO_HISTORY_EMPTY).is_some());
    });
}

#[gpui_kit::test]
fn history_badges_follow_the_status(cx: &mut TestAppContext) {
    let (_fx, handle, _app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.commit(opts("Add b"), w, cx);
    });
    run(cx, handle, &tab, |t, w, cx| {
        t.undo(w, cx);
    });
    let entries = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .backend
            .handle()
            .expect("real")
            .oplog_list(PageRequest {
                before: None,
                limit: 10,
                include_bookkeeping: false,
            })
            .expect("history")
            .entries
    });
    assert_eq!(entries.len(), 1, "the undo entry is bookkeeping and hidden");
    assert_eq!(texts::badges(&entries[0]), ["Undone"]);
    assert!(texts::can_restore(&entries[0], false));
    assert!(texts::can_restore(&entries[0], true));
}

fn undo_depth(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .model
            .read(cx)
            .undo
            .as_ref()
            .map_or(0, |u| u.undo_depth)
    })
}

/// Settings > "Record undo history": while it is off nothing is recorded; turning it back on records a barrier
/// that ends the stacks, and the history list shows it as not undoable (17 §4.4 last row).
#[gpui_kit::test]
fn record_setting_stops_recording_and_a_barrier_ends_the_stacks(cx: &mut TestAppContext) {
    let (fx, handle, _app, tab) = real_tab(cx, true);
    run(cx, handle, &tab, |t, w, cx| {
        t.create_branch("one".to_owned(), StartPoint::Head, false, w, cx);
    });
    assert_eq!(undo_depth(&tab, cx), 1);
    cx.update(|cx| crate::settings::update(cx, |s| s.record_undo_history = false));
    cx.run_until_parked();
    cx.run_until_parked();
    run(cx, handle, &tab, |t, w, cx| {
        t.create_branch("two".to_owned(), StartPoint::Head, false, w, cx);
    });
    assert_eq!(
        undo_depth(&tab, cx),
        1,
        "nothing was recorded while the setting was off"
    );
    assert!(branches(&fx).lines().any(|b| b == "two"));
    cx.update(|cx| crate::settings::update(cx, |s| s.record_undo_history = true));
    cx.run_until_parked();
    cx.run_until_parked();
    cx.run_until_parked();
    assert_eq!(undo_depth(&tab, cx), 0, "the barrier ends the stack");
    assert!(!tips(&tab, cx).0.enabled);
    open_history(cx, handle, &tab);
    let c = controller(&tab, cx);
    let (title, badges) = cx.update(|cx| {
        let p = c.read(cx).history().expect("open").read(cx);
        let e = &p.entries()[0];
        (e.title.clone(), texts::badges(e))
    });
    assert_eq!(title, "Record undo history");
    assert_eq!(badges, ["Not undoable"]);
    frame(cx, handle, |window, cx| {
        window.right_click((ids::UNDO_HISTORY_ROW, 0usize), cx);
    });
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find("menu-item-undo-restore-before").is_none(),
            "a barrier has no recorded state to restore"
        );
    });
}
