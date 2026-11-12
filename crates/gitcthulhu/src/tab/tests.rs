use core::prelude::v1::test;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, Context, Entity, InteractiveElement as _, IntoElement, Modifiers,
    ParentElement as _, Render, ScrollStrategy, StyleRefinement, Styled as _, TestAppContext,
    TestSupportExt as _, UniformListScrollHandle, Window, div, px, uniform_list,
};

use super::model::{ChunkedRows, GraphCell, LogRow, LogSnapshot};
use super::ops::{OpKind, OpSpec, RestoreDone, RestoreKind, refresh_after};
use super::ui::{CenterContent, CloseableView, FileList, RepoUi, RightContent, Selection};
use super::*;
use crate::app_view::AppView;
use crate::backend::handle::TabBackend;
use crate::backend::present::OpFailure;
use crate::backend::prompt::{ConfirmRequest, UiRequest};
use crate::backend::{
    ChangeKind, DiscardMode, GitError, NodeKind, OpOutcome, OpWarning, OperationKind, PageRequest,
    RefreshSet, RelaPath, UndoOutcome,
};
use crate::log_view::graph::{GraphDecoration, PaintRecorder};
use crate::metrics::{self, dp};
use crate::settings::Settings;
use crate::test_support::{
    DEFAULT_SIZE, Fixture, InertBackend, RepoSeed, TestCall, calls, mount_fixture,
    mount_fixture_with, mount_seeded, script_op, toast_texts,
};

fn outcome<T>(value: T, refresh: RefreshSet) -> OpOutcome<T> {
    OpOutcome {
        value,
        refresh,
        log: Default::default(),
        warnings: Vec::new(),
    }
}

fn noop_spec(kind: OpKind) -> OpSpec<()> {
    OpSpec::new(kind, |_| Ok(outcome((), RefreshSet::empty())))
}

fn run(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    spec: OpSpec<()>,
) -> bool {
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| t.run_op(spec, window, cx))
    })
    .expect("window")
}

fn busy(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> bool {
    cx.update(|cx| tab.read(cx).busy().is_some())
}

fn reloads(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<RefreshSet> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|c| match c {
                TestCall::Reload(set) => Some(set),
                _ => None,
            })
            .collect()
    })
}

/// A second seeded tab in the same window.
fn add_seeded_tab(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    app: &Entity<AppView>,
    seed: RepoSeed,
) -> Entity<RepoTab> {
    let tab = cx
        .update_window(handle, |_, window, cx| {
            let tab = cx.new(|cx| {
                RepoTab::new(
                    TabId::next(),
                    TabBackend::Inert(Arc::new(InertBackend::default())),
                    "second".into(),
                    "second".into(),
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
        .expect("window");
    let model = cx.update(|cx| tab.read(cx).ctx().model.clone());
    seed.apply(&model, cx);
    tab
}

#[gpui_kit::test]
fn run_op_rejects_second_op(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::STATUS)),
            Duration::from_secs(1),
        )
    });
    assert!(
        run(cx, handle, &tab, noop_spec(OpKind::StageFile)),
        "the first op starts"
    );
    assert!(
        !run(cx, handle, &tab, noop_spec(OpKind::UnstageFile)),
        "a second op is rejected while the first runs"
    );
    cx.run_until_parked();
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&"Another operation is running".to_owned()),
        "the rejection shows a warning toast"
    );
    assert!(busy(cx, &tab), "still busy before the scripted delay");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(!busy(cx, &tab), "done after the delay");
    assert_eq!(
        cx.update(|cx| calls(&tab, cx)),
        vec![
            TestCall::Op {
                kind: OpKind::StageFile,
                title: None
            },
            TestCall::Reload(RefreshSet::STATUS)
        ],
        "only the first op ran; its outcome's refresh set was reloaded"
    );
}

#[gpui_kit::test]
fn overlay_appears_after_300ms(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::empty())),
            Duration::from_secs(2),
        )
    });
    run(cx, handle, &tab, noop_spec(OpKind::Fetch));
    cx.executor().advance_clock(Duration::from_millis(299));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(ids::PROCESSING_OVERLAY).is_none(),
            "no overlay before 300 ms"
        );
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        crate::test_support::geometry::assert_rect(
            window,
            ids::PROCESSING_OVERLAY,
            0.,
            metrics::TAB_STRIP_H,
            1280.,
            720. - metrics::TAB_STRIP_H,
        );
        assert!(
            window.try_find(crate::shell::ids::PROCESSING).is_some(),
            "the shell's overlay content is rendered inside"
        );
    })
    .expect("window");
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(ids::PROCESSING_OVERLAY).is_none(),
            "the overlay goes away with the operation"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn refresh_mapping_from_outcome(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::STATUS | RefreshSet::LOG)),
            Duration::ZERO,
        )
    });
    let spec = noop_spec(OpKind::Commit)
        .base_refresh(RefreshSet::REPO_STATE)
        .refresh(RefreshSet::TAGS);
    run(cx, handle, &tab, spec);
    cx.run_until_parked();
    assert_eq!(
        reloads(&tab, cx),
        vec![RefreshSet::STATUS | RefreshSet::LOG | RefreshSet::TAGS],
        "success reloads the outcome's set plus the extra set, not the base set"
    );
    let ok: Result<OpOutcome<()>, GitError> = Ok(outcome((), RefreshSet::STATUS));
    assert_eq!(
        refresh_after(&ok, RefreshSet::LOG, RefreshSet::TAGS, true),
        RefreshSet::STATUS | RefreshSet::TAGS | RefreshSet::UNDO,
        "a recorded operation also reloads UNDO"
    );
}

#[gpui_kit::test]
fn refresh_on_failure_uses_base_and_error_sets(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op::<OpOutcome<()>>(
            &tab,
            cx,
            Err(OpFailure::Git(Arc::new(GitError::HookFailed {
                hook: "pre-commit".into(),
                exit_code: Some(1),
                output: String::new(),
            }))),
            Duration::ZERO,
        )
    });
    let spec = noop_spec(OpKind::Commit)
        .base_refresh(RefreshSet::REMOTES)
        .refresh(RefreshSet::TAGS);
    run(cx, handle, &tab, spec);
    cx.run_until_parked();
    assert_eq!(
        reloads(&tab, cx),
        vec![RefreshSet::REMOTES | RefreshSet::STATUS | RefreshSet::TAGS],
        "failure = base ∪ refresh_on_failure (HookFailed: STATUS) ∪ extra"
    );
    let err: Result<OpOutcome<()>, GitError> = Err(GitError::Cancelled);
    assert_eq!(
        refresh_after(&err, RefreshSet::LOG, RefreshSet::empty(), false),
        RefreshSet::LOG,
        "an error without a refresh set reloads the base set"
    );
    cx.update(|cx| script_op::<OpOutcome<()>>(&tab, cx, Err(OpFailure::RepoBusy), Duration::ZERO));
    run(
        cx,
        handle,
        &tab,
        noop_spec(OpKind::Commit).base_refresh(RefreshSet::LOG),
    );
    cx.run_until_parked();
    assert_eq!(
        reloads(&tab, cx).len(),
        1,
        "nothing ran when the guard was busy, so nothing is reloaded"
    );
}

#[gpui_kit::test]
fn uncommitted_row_toggle_does_not_reload_log(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let model = cx.update(|cx| tab.read(cx).ctx().model.clone());
    assert!(
        !cx.update(|cx| model.read(cx).show_uncommitted_row()),
        "a clean repository hides the uncommitted row"
    );
    let rows = cx.update(|cx| model.read(cx).log.value.as_ref().map(|l| l.rows.len()));
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::STATUS)),
            Duration::ZERO,
        )
    });
    run(cx, handle, &tab, noop_spec(OpKind::StageFile));
    cx.run_until_parked();
    let dirty = RepoSeed::linear(3).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    model.update(cx, |m, cx| m.set_status(Arc::new(dirty.status), cx));
    assert!(
        cx.update(|cx| model.read(cx).show_uncommitted_row()),
        "a dirty worktree shows the uncommitted row"
    );
    assert_eq!(
        cx.update(|cx| model.read(cx).log.value.as_ref().map(|l| l.rows.len())),
        rows,
        "the log snapshot is unchanged: row 0 is always laid out"
    );
    assert!(
        reloads(&tab, cx)
            .iter()
            .all(|set| !set.contains(RefreshSet::LOG)),
        "a status change never reloads the log"
    );
    let merging = RepoSeed::linear(3).in_state(crate::backend::InProgress::Revert);
    model.update(cx, |m, cx| {
        m.set_status(Arc::new(RepoSeed::linear(3).status), cx);
        m.set_state(Arc::new(merging.state), cx);
    });
    assert!(
        cx.update(|cx| model.read(cx).show_uncommitted_row()),
        "a revert in progress shows the row (fix of 08 §9.10)"
    );
}

#[gpui_kit::test]
fn no_reentrant_tab_access(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    let seen = Rc::new(Cell::new(0usize));
    cx.update(|cx| {
        let (events, views) = {
            let t = tab.read(cx);
            let v = t.views();
            (
                t.ctx().events.clone(),
                (
                    v.toolbar.clone(),
                    v.side_panel.clone(),
                    v.log.clone(),
                    v.status.clone(),
                    v.commit_changes.clone(),
                    v.diff.clone(),
                    v.bottom_bar.clone(),
                ),
            )
        };
        let weak = tab.downgrade();
        let seen = seen.clone();
        cx.subscribe(&events, move |_, _event: &RepoTabEvent, cx| {
            // Subscribers run after the tab's update: reading the tab and updating views must not panic.
            let tab = weak.upgrade().expect("tab alive");
            let _ = tab.read(cx).busy().is_some();
            views.0.update(cx, |_, cx| cx.notify());
            views.1.update(cx, |_, cx| cx.notify());
            views.2.update(cx, |_, cx| cx.notify());
            views.3.update(cx, |_, cx| cx.notify());
            views.4.update(cx, |_, cx| cx.notify());
            views.5.update(cx, |_, cx| cx.notify());
            views.6.update(cx, |_, cx| cx.notify());
            seen.set(seen.get() + 1);
        })
        .detach();
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::empty())),
            Duration::ZERO,
        );
    });
    let spec = noop_spec(OpKind::Commit).then(|tab, _, _, cx| {
        let id = crate::test_support::seed::oid(1);
        for event in [
            RepoTabEvent::StatusReloaded,
            RepoTabEvent::LogReloaded { generation: 7 },
            RepoTabEvent::ScrollToCommit { id },
            RepoTabEvent::OpStarted {
                kind: OpKind::Commit,
            },
            RepoTabEvent::OpFinished {
                kind: OpKind::Commit,
                ok: true,
            },
            RepoTabEvent::FocusCommitMessage,
            RepoTabEvent::ReloadDiff,
        ] {
            tab.emit(event, cx);
        }
        tab.ctx.ui.update(cx, |ui, cx| {
            ui.select(
                Selection::Commit {
                    id,
                    is_stash: false,
                    scroll_to: true,
                },
                cx,
            )
        });
    });
    run(cx, handle, &tab, spec);
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    assert_eq!(
        seen.get(),
        9,
        "OpStarted + the seven events of `then` + OpFinished reached the subscriber"
    );
}

#[gpui_kit::test]
fn post_hook_warning_toast_opens_output(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update(|cx| {
        let mut o = outcome((), RefreshSet::empty());
        o.warnings.push(OpWarning::PostHookFailed {
            hook: "post-checkout".into(),
            exit_code: Some(1),
            output: "hook says no".into(),
        });
        script_op(&tab, cx, Ok(o), Duration::ZERO);
    });
    run(cx, handle, &tab, noop_spec(OpKind::CheckoutBranch));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| toast_texts(&app, cx)),
        vec!["Branch checked out - the post-checkout hook failed (exit code 1)".to_owned()],
        "a Warning toast replaces the success toast"
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(("toast-action", 0usize), cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(crate::dialogs::error::ids::ERROR_HOOK_OUTPUT)
                .is_some(),
            "the warning-mode error dialog shows the hook output"
        );
    })
    .expect("window");
    assert_eq!(
        cx.update(|cx| app.read(cx).errors_opened.clone()),
        vec![gpui_kit::SharedString::from("Hook failed")],
        "\"Show output\" opened the warning-mode dialog"
    );
}

#[gpui_kit::test]
fn ui_request_selects_requesting_tab(cx: &mut TestAppContext) {
    let (handle, app, first) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let _second = add_seeded_tab(cx, handle, &app, RepoSeed::linear(2));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| app.read(cx).active()),
        1,
        "the second tab is selected"
    );
    let first_id = cx.update(|cx| first.read(cx).id());
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    cx.update(|cx| {
        crate::app::ui_requests(cx)
            .expect("request pump")
            .send(UiRequest::Confirm {
                tab: Some(first_id),
                request: ConfirmRequest::new("Question", "Continue?"),
                reply,
            })
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| app.read(cx).active()),
        0,
        "the asking tab is selected before the dialog opens"
    );
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    assert_eq!(
        answer.try_recv(),
        Ok(true),
        "the confirmation answers the background thread"
    );
}

#[gpui_kit::test]
fn restore_outcomes_are_presented(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op::<RestoreDone>(
            &tab,
            cx,
            Ok(RestoreDone::Outcome(UndoOutcome::Nothing)),
            Duration::ZERO,
        )
    });
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| assert!(t.undo(window, cx), "undo starts"))
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&"Nothing to undo".to_owned())
    );
    assert_eq!(
        cx.update(|cx| calls(&tab, cx)),
        vec![
            TestCall::Restore(RestoreKind::Undo),
            TestCall::Reload(RefreshSet::ALL)
        ],
        "a restore reloads everything"
    );
}

#[gpui_kit::test]
fn escape_in_search_field_closes_search(cx: &mut TestAppContext) {
    // The kit input lets its Escape action propagate to the frame...
    crate::test_support::init_app(cx);
    struct Frame {
        field: Entity<crate::widgets::field::SearchField>,
        escapes: Rc<Cell<usize>>,
    }
    impl Render for Frame {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let escapes = self.escapes.clone();
            div()
                .size_full()
                .key_context(crate::actions::REPO_CONTEXT)
                .on_action(move |_: &gpui_kit::base::input::Escape, _, _| {
                    escapes.set(escapes.get() + 1)
                })
                .child(self.field.clone())
        }
    }
    let escapes = Rc::new(Cell::new(0));
    let counter = escapes.clone();
    let (handle, frame) = cx.update(|cx| {
        gpui_kit::open_window(
            crate::test_support::geometry::window_options(800., 600.),
            cx,
            move |window, cx| {
                let field = cx.new(|cx| {
                    crate::widgets::field::SearchField::new("probe-search", "Search", window, cx)
                });
                cx.new(|_| Frame {
                    field,
                    escapes: counter,
                })
            },
        )
        .expect("window")
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let field = frame.read(cx).field.clone();
        field.update(cx, |f, cx| f.focus(window, cx));
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .expect("window");
    assert_eq!(
        escapes.get(),
        1,
        "Escape in a focused search field reaches the frame"
    );

    // ...and the repository frame closes the most recent closeable view on it.
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    ui.update(cx, |ui, cx| {
        ui.log_search = Some(Default::default());
        ui.push_view(CloseableView::LogSearch, cx);
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        tab.update(cx, |t, cx| t.focus(window, cx));
        window.render_frame(cx);
        window.dispatch_action(Box::new(gpui_kit::base::input::Escape), cx);
    })
    .expect("window");
    cx.update(|cx| {
        let ui = ui.read(cx);
        assert!(ui.log_search.is_none(), "the log search was closed");
        assert!(ui.views.as_slice().is_empty(), "the view stack is empty");
    });
}

#[gpui_kit::test]
fn ancestor_keys_table_matches_bindings(cx: &mut TestAppContext) {
    use crate::actions::{ANCESTOR_KEYS, APP_CONTEXT, REPO_CONTEXT, bindings, processing_bindings};
    let expected: &[(&str, &str)] = &[
        ("secondary-t", APP_CONTEXT),
        ("secondary-w", APP_CONTEXT),
        ("alt-left", APP_CONTEXT),
        ("ctrl-shift-tab", APP_CONTEXT),
        ("alt-right", APP_CONTEXT),
        ("ctrl-tab", APP_CONTEXT),
        ("secondary-o", APP_CONTEXT),
        ("secondary-alt-s", APP_CONTEXT),
        ("secondary-u", REPO_CONTEXT),
        ("secondary-p", REPO_CONTEXT),
        ("secondary-b", REPO_CONTEXT),
        ("secondary-s", REPO_CONTEXT),
        ("secondary-shift-s", REPO_CONTEXT),
        ("f5", REPO_CONTEXT),
        ("secondary-r", REPO_CONTEXT),
        ("escape", REPO_CONTEXT),
        ("secondary-z", "RepoView && !Input"),
        ("secondary-shift-z", "RepoView && !Input"),
        ("ctrl-y", "RepoView && !Input"),
        ("ctrl-`", REPO_CONTEXT),
    ];
    assert_eq!(
        ANCESTOR_KEYS, expected,
        "the table lists every spec 21 §2.12 binding"
    );
    let bound = bindings();
    let platform_keys: Vec<&(&str, &str)> = ANCESTOR_KEYS
        .iter()
        .filter(|(k, _)| !(cfg!(target_os = "macos") && matches!(*k, "alt-left" | "alt-right")))
        .collect();
    assert_eq!(
        bound.len(),
        platform_keys.len(),
        "one binding per table entry"
    );
    let processing = processing_bindings();
    assert_eq!(
        processing.len(),
        ANCESTOR_KEYS.len(),
        "the overlay swallows every ancestor key (NoAction) and maps Escape to Cancel"
    );
    crate::test_support::init_app(cx);
    cx.update(|cx| {
        for action in [
            Box::new(crate::actions::Refresh) as Box<dyn gpui_kit::Action>,
            Box::new(crate::actions::NewTab),
            Box::new(crate::actions::Undo),
            Box::new(crate::actions::ToggleTerminal),
        ] {
            assert!(
                !cx.key_bindings()
                    .borrow()
                    .bindings_for_action(action.as_ref())
                    .collect::<Vec<_>>()
                    .is_empty(),
                "{action:?} is bound after actions::init"
            );
        }
    });
}

#[gpui_kit::test]
fn view_stack_lifo(cx: &mut TestAppContext) {
    let ui = cx.new(|_| RepoUi::new());
    ui.update(cx, |ui, cx| {
        ui.side_filter = "x".into();
        ui.log_search = Some(Default::default());
        ui.push_view(CloseableView::SidePaneSearch, cx);
        ui.push_view(CloseableView::LogSearch, cx);
        ui.push_view(CloseableView::Blame, cx);
        ui.push_view(CloseableView::SidePaneSearch, cx);
        assert_eq!(
            ui.views.as_slice(),
            &[
                CloseableView::LogSearch,
                CloseableView::Blame,
                CloseableView::SidePaneSearch
            ],
            "pushing an open view moves it to the top"
        );
        assert!(ui.close_last_view(cx));
        assert!(
            ui.side_filter.is_empty(),
            "closing the side search clears its filter"
        );
        assert!(ui.close_last_view(cx));
        assert!(ui.close_last_view(cx));
        assert!(ui.log_search.is_none(), "LogSearch closes last");
        assert!(!ui.close_last_view(cx), "an empty stack reports false");
    });
}

#[gpui_kit::test]
fn click_file_selection_matrix(cx: &mut TestAppContext) {
    let ui = cx.new(|_| RepoUi::new());
    let p = |s: &str| RelaPath::from(s);
    let order: Vec<RelaPath> = ["a", "b", "c", "d", "e"].iter().map(|s| p(s)).collect();
    let none = Modifiers::default();
    let ctrl = Modifiers::secondary_key();
    let shift = Modifiers::shift();
    ui.update(cx, |ui, cx| {
        let selected =
            |ui: &RepoUi| -> Vec<String> { ui.files.paths.iter().map(|p| p.to_string()).collect() };
        ui.click_file(FileList::Unstaged, p("b"), none, &order, cx);
        assert_eq!(selected(ui), ["b"], "plain click selects one file");
        ui.click_file(FileList::Unstaged, p("d"), ctrl, &order, cx);
        assert_eq!(selected(ui), ["b", "d"], "Ctrl adds");
        ui.click_file(FileList::Unstaged, p("b"), ctrl, &order, cx);
        assert_eq!(selected(ui), ["d"], "Ctrl toggles off");
        ui.click_file(FileList::Unstaged, p("a"), shift, &order, cx);
        assert_eq!(
            selected(ui),
            ["d", "c", "b", "a"],
            "Shift adds the range from the most recently added path"
        );
        ui.click_file(FileList::Staged, p("x"), ctrl, &order, cx);
        assert_eq!(
            (ui.files.list, selected(ui)),
            (Some(FileList::Staged), vec!["x".to_owned()]),
            "a click in another list replaces the selection there"
        );
        ui.click_file(FileList::Unstaged, p("e"), shift, &order, cx);
        assert_eq!(
            selected(ui),
            ["e"],
            "Shift in a new list acts as a plain click"
        );
        ui.click_file(FileList::CommitFiles, p("a"), none, &order, cx);
        ui.click_file(FileList::CommitFiles, p("b"), ctrl, &order, cx);
        assert_eq!(
            selected(ui),
            ["b"],
            "commit files are single selection only"
        );
        ui.clear_files(cx);
        assert!(ui.files.list.is_none(), "cleared");
    });
}

#[gpui_kit::test]
fn center_and_right_content_rules(cx: &mut TestAppContext) {
    let model = cx.new(|_| super::model::RepoModel::default());
    let seed = RepoSeed::linear(2).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    model.update(cx, |m, cx| m.set_status(Arc::new(seed.status), cx));
    let ui = cx.new(|_| RepoUi::new());
    cx.update(|cx| {
        let settings = Settings::default();
        let (u, m) = (ui.read(cx), model.read(cx));
        assert_eq!(
            u.center(&settings, m),
            CenterContent::LogAndDiff {
                log: true,
                diff: None,
                minimized_blame: false
            }
        );
        assert_eq!(
            u.right(),
            RightContent::Status,
            "uncommitted changes show the status pane"
        );
    });
    ui.update(cx, |ui, cx| {
        ui.click_file(
            FileList::Unstaged,
            RelaPath::from("a.txt"),
            Modifiers::default(),
            &[],
            cx,
        )
    });
    cx.update(|cx| {
        let (u, m) = (ui.read(cx), model.read(cx));
        let center = u.center(&Settings::default(), m);
        assert!(
            matches!(
                &center,
                CenterContent::LogAndDiff {
                    log: false,
                    diff: Some(_),
                    ..
                }
            ),
            "one selected file replaces the log by the diff: {center:?}"
        );
        let with_log = Settings {
            show_diff_with_log: true,
            ..Settings::default()
        };
        assert!(matches!(
            u.center(&with_log, m),
            CenterContent::LogAndDiff {
                log: true,
                diff: Some(_),
                ..
            }
        ));
    });
    let id = crate::test_support::seed::oid(5);
    ui.update(cx, |ui, cx| {
        ui.select(Selection::Stash { id, index: 0 }, cx)
    });
    cx.update(|cx| {
        let u = ui.read(cx);
        assert_eq!(
            u.right(),
            RightContent::CommitChanges {
                commit: id,
                stash: true
            }
        );
        assert!(
            u.files.list.is_none(),
            "a new right pane clears the file selection"
        );
    });
}

#[gpui_kit::test]
fn frame_regions_at_final_bounds(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        use crate::ids;
        use crate::test_support::geometry::{assert_height, assert_rect, assert_width};
        let top = metrics::TAB_STRIP_H;
        assert_rect(window, ids::TAB_STRIP, 0., 0., 1280., metrics::TAB_STRIP_H);
        assert_rect(window, ids::TOOLBAR, 0., top, 1280., metrics::TOOLBAR_H);
        assert_rect(
            window,
            ids::BOTTOM_BAR,
            0.,
            720. - metrics::BOTTOM_BAR_H,
            1280.,
            metrics::BOTTOM_BAR_H,
        );
        assert_height(window, ids::FRAME_DIVIDER, metrics::FRAME_DIVIDER_H);
        assert_height(window, ids::TERMINAL_PANEL, 0.);
        assert_width(window, ids::PANE_FIRST, metrics::SIDE_PANE_W);
        assert_width(window, ids::GUTTER_FIRST, metrics::SPLIT_GUTTER);
        assert_width(window, ids::PANE_THIRD, metrics::RIGHT_PANE_W);
        let split_h =
            720. - top - metrics::TOOLBAR_H - metrics::FRAME_DIVIDER_H - metrics::BOTTOM_BAR_H;
        assert_rect(
            window,
            crate::side_panel::ids::SIDE_PANEL,
            0.,
            top + metrics::TOOLBAR_H,
            metrics::SIDE_PANE_W,
            split_h,
        );
        assert_rect(
            window,
            crate::log_view::ids::LOG_VIEW,
            metrics::SIDE_PANE_W + metrics::SPLIT_GUTTER,
            top + metrics::TOOLBAR_H,
            1280. - metrics::SIDE_PANE_W - metrics::RIGHT_PANE_W - 2. * metrics::SPLIT_GUTTER,
            split_h,
        );
        assert_rect(
            window,
            crate::status_pane::ids::STATUS_PANE,
            1280. - metrics::RIGHT_PANE_W,
            top + metrics::TOOLBAR_H,
            metrics::RIGHT_PANE_W,
            split_h,
        );
        // D0 slots (§8.2): Undo/Redo present (disabled), terminal panel zero height.
        assert_rect(
            window,
            crate::undo::ids::TOOLBAR_UNDO,
            bounds_x(window, crate::undo::ids::TOOLBAR_UNDO),
            top + metrics::TOOLBAR_PAD_V,
            metrics::EXT_MENU_BUTTON_W,
            metrics::MENU_BUTTON,
        );
        assert_width(window, crate::undo::ids::TOOLBAR_REDO, metrics::MENU_BUTTON);
    })
    .expect("window");
}

/// §8.2 Split row: dragging the frame's gutters clamps to [150, 0.4 W], the frame follows live, and the widths are
/// persisted when the drag ends.
#[gpui_kit::test]
fn frame_split_drag_clamps_and_persists(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    cx.update_window(handle, |_, window, cx| {
        use crate::test_support::geometry::{assert_width, bounds};
        window.render_frame(cx);
        let grab = bounds(window, crate::ids::GUTTER_FIRST).center();
        window.drag(grab, gpui_kit::point(grab.x + px(900.), grab.y), cx);
        window.render_frame(cx);
        assert_width(window, crate::ids::PANE_FIRST, 512.);
        let grab = bounds(window, crate::ids::GUTTER_THIRD).center();
        window.drag(grab, gpui_kit::point(grab.x - px(40.), grab.y), cx);
        window.render_frame(cx);
        assert_width(window, crate::ids::PANE_THIRD, 370.);
        let stored = crate::settings::state::get(cx);
        assert_eq!(
            (stored.first_pane_width, stored.third_pane_width),
            (512., 370.),
            "the widths are persisted when a drag ends"
        );
        assert!(
            tab.read(cx).drag_widths.is_none(),
            "the live drag state ends with the drag"
        );
    })
    .expect("window");
}

/// §2.5: an undo-settings change is applied to every open tab; while an operation runs it is queued until the
/// operation finishes.
#[gpui_kit::test]
fn undo_settings_apply_to_open_tabs(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let applied = |tab: &Entity<RepoTab>, cx: &mut TestAppContext| {
        cx.update(|cx| {
            calls(tab, cx)
                .into_iter()
                .filter(|c| {
                    matches!(
                        c,
                        TestCall::Op {
                            kind: OpKind::ApplyUndoSettings,
                            ..
                        }
                    )
                })
                .count()
        })
    };
    cx.update(|cx| crate::settings::update(cx, |s| s.undo_keep_entries = 42));
    cx.run_until_parked();
    assert_eq!(applied(&tab, cx), 1, "the change was applied to the tab");
    assert_eq!(
        cx.update(|cx| tab.read(cx).applied_undo_settings().keep_entries),
        42
    );
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(outcome((), RefreshSet::empty())),
            Duration::from_secs(1),
        )
    });
    run(cx, handle, &tab, noop_spec(OpKind::Commit));
    cx.update(|cx| crate::settings::update(cx, |s| s.record_undo_history = false));
    cx.run_until_parked();
    assert_eq!(applied(&tab, cx), 1, "queued while the operation runs");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(applied(&tab, cx), 2, "applied when the operation finished");
}

/// §2.5: undo-settings applications run one at a time per tab. A burst of changes (typing a number, toggling twice)
/// is queued behind the running application, so the oplog ends with the newest settings; turning recording back on
/// records the barrier.
#[gpui_kit::test]
fn undo_settings_apply_one_at_a_time(cx: &mut TestAppContext) {
    use crate::backend::handle::UndoSettings;
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (_handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let on = UndoSettings::default();
    let off = UndoSettings {
        enabled: false,
        ..on
    };
    let off_7 = UndoSettings {
        keep_entries: 7,
        ..off
    };
    tab.update(cx, |t, cx| {
        t.apply_undo_settings(true, off, cx);
        assert!(t.undo.is_applying(), "the first change is applied at once");
        t.apply_undo_settings(false, on, cx);
        t.apply_undo_settings(true, off_7, cx);
        assert_eq!(
            t.undo.queued,
            Some((false, off_7)),
            "later changes wait for the running one; the oldest 'was enabled' is kept"
        );
    });
    cx.run_until_parked();
    let enabled = cx.update(|cx| {
        let t = tab.read(cx);
        assert!(
            t.undo.queued.is_none() && !t.undo.is_applying(),
            "the queue drained"
        );
        assert_eq!(t.applied_undo_settings(), off_7);
        t.ctx().backend.handle().expect("real").oplog_enabled()
    });
    assert!(!enabled, "the newest settings (recording off) won");
    let barriers = |tab: &Entity<RepoTab>, cx: &mut TestAppContext| {
        oplog_entries(tab, cx)
            .iter()
            .filter(|e| e.kind == OperationKind::Barrier)
            .count()
    };
    assert_eq!(barriers(&tab, cx), 0, "recording stayed off: no barrier");

    tab.update(cx, |t, cx| t.apply_undo_settings(false, on, cx));
    cx.run_until_parked();
    assert!(
        cx.update(|cx| tab
            .read(cx)
            .ctx()
            .backend
            .handle()
            .expect("real")
            .oplog_enabled()),
        "recording is on again"
    );
    assert_eq!(
        barriers(&tab, cx),
        1,
        "turning recording back on records the barrier"
    );
}

/// The clone API of the clone dialog (09 §3.1): progress stream, outcome callback; failures go to the dialog, not to
/// the error queue, and the slot returns to the welcome page.
#[gpui_kit::test]
fn clone_reports_its_outcome_to_the_caller(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let target = fx.dir.path().join("clone");
    let opts = crate::backend::CloneOptions {
        url: fx.path().to_string_lossy().into_owned(),
        directory: target.clone(),
        branch: None,
        depth: None,
        recurse_submodules: true,
        bare: false,
    };
    let start = |cx: &mut TestAppContext, opts: crate::backend::CloneOptions| {
        let done: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
        let sink = done.clone();
        let job = cx
            .update_window(handle, |_, window, cx| {
                app.update(cx, |app, cx| {
                    app.clone_repository(
                        opts,
                        Box::new(move |result, _, _| {
                            *sink.borrow_mut() = Some(result.map_err(|e| e.to_string()))
                        }),
                        window,
                        cx,
                    )
                })
            })
            .expect("window");
        cx.run_until_parked();
        (done, job)
    };

    let (done, mut job) = start(cx, opts.clone());
    assert_eq!(*done.borrow(), Some(Ok(())), "the clone succeeded");
    let end = loop {
        match job.progress.try_recv() {
            Ok(_) => continue,
            Err(end) => break end,
        }
    };
    assert_eq!(
        end,
        futures::channel::mpsc::TryRecvError::Closed,
        "the progress stream ended with the clone"
    );
    assert!(target.join("a.txt").exists(), "the files were checked out");
    cx.update(|cx| {
        let app = app.read(cx);
        let tab = app
            .active_tab()
            .expect("the clone opened in the selected tab");
        assert!(
            tab.read(cx).path().ends_with("clone"),
            "the selected tab is the clone"
        );
    });

    let tabs_before = cx.update(|cx| app.read(cx).tabs().len());
    let (done, _job) = start(cx, opts);
    assert!(
        matches!(&*done.borrow(), Some(Err(_))),
        "cloning into a non-empty directory fails"
    );
    cx.update(|cx| {
        let app = app.read(cx);
        assert!(
            app.errors_opened.is_empty() && app.pending_errors() == 0,
            "the dialog shows the failure; nothing is queued as an error dialog"
        );
        assert_eq!(
            app.tabs().len(),
            tabs_before,
            "the slot the failed clone added next to a repository tab is removed again (no extra 'New tab')"
        );
        assert!(
            app.active_tab()
                .is_some_and(|tab| tab.read(cx).path().ends_with("clone")),
            "the repository tab it was started from is selected again"
        );
    });
}

/// 09 §1.1 / gpui-kit pitfall: the repository frame focuses itself on clicks into empty areas, but must not take the
/// focus from a text field that was clicked (the frame and AppView both `track_focus`).
#[gpui_kit::test]
fn click_into_field_keeps_focus_in_frame(cx: &mut TestAppContext) {
    crate::test_support::init_app(cx);
    struct Frame {
        outer: gpui_kit::FocusHandle,
        frame: gpui_kit::FocusHandle,
        field: Entity<crate::widgets::field::OutlinedField>,
    }
    impl Render for Frame {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // The same structure as AppView (outer) + RepoTab's render (frame).
            div().size_full().track_focus(&self.outer).child(
                div()
                    .id("probe-frame")
                    .size_full()
                    .key_context(crate::actions::REPO_CONTEXT)
                    .track_focus(&self.frame)
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if !this.frame.contains_focused(window, cx) {
                                window.focus(&this.frame, cx);
                            }
                        }),
                    )
                    .child(div().w(px(300.)).child(self.field.clone()))
                    .child(div().id("probe-empty").h(px(200.)).w_full().test_support())
                    .test_support(),
            )
        }
    }
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            crate::test_support::geometry::window_options(800., 600.),
            cx,
            |window, cx| {
                let field = cx
                    .new(|cx| crate::widgets::field::OutlinedField::new("probe-field", window, cx));
                cx.new(|cx| Frame {
                    outer: cx.focus_handle(),
                    frame: cx.focus_handle(),
                    field,
                })
            },
        )
        .expect("window")
    });
    cx.update_window(handle, |_, window, cx| {
        use crate::widgets::field::BorderState;
        window.render_frame(cx);
        window.click("probe-field", cx);
        window.render_frame(cx);
        let field = view.read(cx).field.clone();
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Focused,
            "a click into the field focuses it, neither the frame nor AppView steal it"
        );
        window.click("probe-empty", cx);
        window.render_frame(cx);
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Unfocused,
            "a click into an empty area moves the focus away from the field"
        );
        assert!(
            view.read(cx).frame.is_focused(window),
            "... to the repository frame"
        );
    })
    .expect("window");
}

fn bounds_x(window: &Window, id: &'static str) -> f32 {
    crate::test_support::geometry::bounds(window, id)
        .origin
        .x
        .as_f32()
}

// ── probes ────────────────────────────────────────────────────────────────────────────────────────

struct HoverBox;

impl Render for HoverBox {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("hover-box")
            .w(px(100.))
            .h(px(20.))
            .hover(|s| s.w(px(200.)))
            .test_support()
    }
}

struct CachedHost {
    inner: Entity<HoverBox>,
}

impl Render for CachedHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            self.inner
                .clone()
                .cached(StyleRefinement::default().size_full()),
        )
    }
}

/// Spec 21 §2.1 rule 6: hover styles keep updating inside an `Entity::cached` view, so large panes may be cached
/// (G12 records the decision from this probe).
#[gpui_kit::test]
fn cached_view_hover_updates(cx: &mut TestAppContext) {
    crate::test_support::init_app(cx);
    let (handle, _) = cx.update(|cx| {
        gpui_kit::open_window(
            crate::test_support::geometry::window_options(800., 600.),
            cx,
            |_, cx| {
                let inner = cx.new(|_| HoverBox);
                cx.new(|_| CachedHost { inner })
            },
        )
        .expect("window")
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        crate::test_support::geometry::assert_width(window, "hover-box", 100.);
        window.hover("hover-box", cx);
        window.render_frame(cx);
        window.render_frame(cx);
        crate::test_support::geometry::assert_width(window, "hover-box", 200.);
    })
    .expect("window");
}

struct DecoratedLog {
    snapshot: Arc<LogSnapshot>,
    handle: UniformListScrollHandle,
    recorder: PaintRecorder,
}

impl Render for DecoratedLog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::ActiveTokens as _;
        let lanes = cx.theme_g().lanes;
        let count = self.snapshot.rows.len();
        uniform_list("log", count, |range, _, _| {
            range
                .map(|ix| {
                    div()
                        .id((crate::log_view::ids::LOG_ROW, ix))
                        .h(dp(38.))
                        .w_full()
                        .test_support()
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.handle)
        .with_decoration(GraphDecoration {
            snapshot: self.snapshot.clone(),
            offset: 0,
            show_uncommitted: true,
            graph_w: px(120.),
            hscroll: px(0.),
            lanes,
            recorder: Some(self.recorder.clone()),
        })
        .h(px(600.))
        .w_full()
    }
}

/// Spec 21 §8.1 / §4.2: the reference decoration paints row tops (f64 formula) that match the rows' real bounds
/// near row 1,000,000.
#[gpui_kit::test]
fn decoration_row_top_matches_rows(cx: &mut TestAppContext) {
    crate::test_support::init_app(cx);
    let rows: Vec<LogRow> = (0..1_000_000usize)
        .map(|i| {
            let mut graph = GraphCell::node((i % 3) as u16, NodeKind::Commit);
            graph.line_up = true;
            graph.line_down = true;
            LogRow {
                commit: None,
                graph,
            }
        })
        .collect();
    let snapshot = Arc::new(LogSnapshot {
        rows: ChunkedRows::new().appended(rows),
        max_lane: 2,
        exhausted: true,
        generation: 1,
    });
    let recorder: PaintRecorder = Rc::new(RefCell::new(Vec::new()));
    let view_recorder = recorder.clone();
    let (handle, view) = cx.update(|cx| {
        gpui_kit::open_window(
            crate::test_support::geometry::window_options(800., 600.),
            cx,
            move |_, cx| {
                cx.new(|_| DecoratedLog {
                    snapshot,
                    handle: UniformListScrollHandle::new(),
                    recorder: view_recorder,
                })
            },
        )
        .expect("window")
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        view.read(cx)
            .handle
            .scroll_to_item(990_000, ScrollStrategy::Top);
        recorder.borrow_mut().clear();
        window.render_frame(cx);
        let painted = recorder.borrow().clone();
        assert!(
            painted.iter().any(|(ix, _)| *ix == 990_000),
            "the decoration painted the scrolled-to rows"
        );
        let mut compared = 0;
        for (ix, top) in painted {
            if let Some(row) = window.try_find((crate::log_view::ids::LOG_ROW, ix)) {
                let real = row.bounds().origin.y;
                assert!(
                    (top - real).abs() <= px(0.5),
                    "row {ix}: painted top {top:?}, real top {real:?}"
                );
                compared += 1;
            }
        }
        assert!(
            compared >= 10,
            "a viewport of rows was compared ({compared})"
        );
    })
    .expect("window");
}

// ── fixture tests (real git) ───────────────────────────────────────────────────────────────────────

fn oplog_entries(
    tab: &Entity<RepoTab>,
    cx: &mut TestAppContext,
) -> Vec<crate::backend::OplogEntry> {
    cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .backend
            .handle()
            .expect("real backend")
            .oplog_list(PageRequest {
                before: None,
                limit: 50,
                include_bookkeeping: true,
            })
            .map(|p| p.entries)
            .unwrap_or_default()
    })
}

fn status_paths(tab: &Entity<RepoTab>, cx: &mut TestAppContext, staged: bool) -> Vec<String> {
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.read(cx);
        let status = model.status.value.as_ref().expect("status loaded");
        let list = if staged {
            &status.staged
        } else {
            &status.unstaged
        };
        list.iter().map(|e| e.path.to_string()).collect()
    })
}

fn run_on<T>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    f: impl FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>) -> T,
) -> T {
    let result = cx
        .update_window(handle, |_, window, cx| {
            tab.update(cx, |t, cx| f(t, window, cx))
        })
        .expect("window");
    cx.run_until_parked();
    result
}

#[gpui_kit::test]
fn fixture_stage_paths_records_nothing(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    assert_eq!(
        status_paths(&tab, cx, false),
        ["a.txt"],
        "the change is unstaged"
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.stage_paths(vec!["a.txt".into()], w, cx)
    });
    assert_eq!(
        status_paths(&tab, cx, true),
        ["a.txt"],
        "the file is staged"
    );
    assert!(
        oplog_entries(&tab, cx).is_empty(),
        "staging is not recorded"
    );
}

#[gpui_kit::test]
fn fixture_discard_untracked_records_snapshot_with_paths(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("new.txt", "fresh\n");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.discard_paths(
            vec!["new.txt".into()],
            DiscardMode::StagedAndUnstaged,
            vec!["new.txt".into()],
            w,
            cx,
        )
    });
    assert!(
        !fx.path().join("new.txt").exists(),
        "the untracked file was deleted"
    );
    let entries = oplog_entries(&tab, cx);
    assert_eq!(entries.len(), 1, "one snapshot entry");
    assert_eq!(entries[0].kind, OperationKind::Discard);
    assert_eq!(entries[0].title, "Discard changes in 'new.txt'");
    assert!(
        entries[0].skipped_untracked.is_empty(),
        "the file was captured"
    );
    let undo = cx.update(|cx| tab.read(cx).ctx().model.read(cx).undo.clone());
    assert!(
        undo.and_then(|u| u.undo).is_some(),
        "the UNDO reload sees the undoable entry"
    );
}

#[gpui_kit::test]
fn fixture_prepare_decline_runs_nothing(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("new.txt", "keep me\n");
    let (handle, _app, tab) = mount_fixture_with(cx, &fx, DEFAULT_SIZE, false);
    let off = crate::backend::handle::UndoSettings {
        capture_untracked: false,
        ..crate::backend::handle::UndoSettings::default()
    };
    run_on(cx, handle, &tab, |t, _, cx| {
        t.apply_undo_settings(true, off, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.discard_paths(
            vec!["new.txt".into()],
            DiscardMode::StagedAndUnstaged,
            vec!["new.txt".into()],
            w,
            cx,
        )
    });
    assert!(
        fx.path().join("new.txt").exists(),
        "declining kept the file"
    );
    assert!(oplog_entries(&tab, cx).is_empty(), "nothing was recorded");
    assert!(!busy(cx, &tab), "the operation ended");
}

#[gpui_kit::test]
fn fixture_try_lock_timeout_shows_repo_busy(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let repo = cx.update(|cx| tab.read(cx).ctx().backend.repo().cloned().expect("real"));
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = repo.lock_exclusive();
        locked_tx.send(()).expect("test alive");
        std::thread::sleep(Duration::from_secs(3));
    });
    locked_rx.recv().expect("the other thread holds the guard");
    run_on(cx, handle, &tab, |t, w, cx| {
        t.stage_paths(vec!["a.txt".into()], w, cx)
    });
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&crate::backend::present::REPO_BUSY.to_owned()),
        "a held guard ends the operation with the RepoBusy warning"
    );
    assert_eq!(
        status_paths(&tab, cx, false),
        ["a.txt"],
        "nothing was staged"
    );
    holder.join().expect("holder thread");
}

#[gpui_kit::test]
fn fixture_remove_worktree_clears_oplog_ref(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let wt = fx.dir.path().join("wt");
    let wt_arg = wt.to_string_lossy().into_owned();
    fx.git(&["worktree", "add", "-q", "-b", "side", &wt_arg]);
    fx.git(&["update-ref", "refs/gitcthulhu/oplog/linked/wt", "HEAD"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.remove_worktree(wt.clone(), false, w, cx)
    });
    assert!(!wt.exists(), "the worktree directory is gone");
    let refs = fx.git(&["for-each-ref", "refs/gitcthulhu/"]);
    assert!(
        !refs.contains("linked/wt"),
        "the removed worktree's oplog chain was cleared: {refs}"
    );
}

#[gpui_kit::test]
fn fixture_push_rejection_records_marker(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let remote = fx.dir.path().join("remote.git");
    let other = fx.dir.path().join("other");
    let (remote_arg, other_arg) = (
        remote.to_string_lossy().into_owned(),
        other.to_string_lossy().into_owned(),
    );
    fx.git(&["init", "-q", "--bare", "-b", "main", &remote_arg]);
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["remote", "add", "origin", &remote_arg]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    fx.git(&["clone", "-q", &remote_arg, &other_arg]);
    fx.git(&[
        "-C",
        &other_arg,
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "theirs",
    ]);
    fx.git(&["-C", &other_arg, "push", "-q", "origin", "main"]);
    fx.write("a.txt", "mine\n");
    fx.commit_all("mine");
    fx.git(&["tag", "v1"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, true, w, cx)
    });
    let markers: Vec<_> = oplog_entries(&tab, cx)
        .into_iter()
        .filter(|e| e.kind == OperationKind::PushTags)
        .collect();
    assert_eq!(
        markers.len(),
        1,
        "the accepted tag was recorded as a marker"
    );
    let network = markers[0].network.clone().expect("a network marker");
    assert!(
        network
            .updates
            .iter()
            .all(|u| u.remote_ref.as_bstr() == "refs/tags/v1"),
        "only the accepted tag is in the marker: {network:?}"
    );
}

#[gpui_kit::test]
fn log_reload_coalesces_with_dirty_flag(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    fx.write("a.txt", "two\n");
    fx.commit_all("second");
    let (_handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let generation = cx.update(|cx| tab.read(cx).loader.log_generation());
    tab.update(cx, |t, cx| {
        t.reload(RefreshSet::LOG, cx);
        assert!(t.loader.log_loading(), "a LOG load runs");
        t.reload(RefreshSet::LOG, cx);
        assert!(
            t.loader.log_dirty(),
            "a second request only marks the load dirty"
        );
        assert_eq!(
            t.loader.log_generation(),
            generation + 1,
            "the running load was not restarted"
        );
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let t = tab.read(cx);
        assert!(
            !t.loader.log_dirty() && !t.loader.log_loading(),
            "both loads finished"
        );
        assert_eq!(
            t.loader.log_generation(),
            generation + 2,
            "the dirty flag ran one more load"
        );
        let model = t.ctx().model.read(cx);
        let log = model.log.value.as_ref().expect("log loaded");
        assert_eq!(
            log.generation,
            generation + 2,
            "the newest snapshot is applied"
        );
        assert_eq!(log.rows.len(), 3, "row 0 plus two commits");
        assert!(
            log.rows.get(0).is_some_and(|r| r.commit.is_none()),
            "row 0 is the uncommitted row"
        );
        assert!(model.log_pager.is_some(), "the pager is kept for paging");
    });
}

#[gpui_kit::test]
fn fixture_commit_records_entry_and_reloads(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.git(&["add", "a.txt"]);
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.commit(
            crate::backend::CommitOptions {
                message: "Second commit\n\nbody".into(),
                ..Default::default()
            },
            w,
            cx,
        )
    });
    assert_eq!(
        fx.git(&["log", "-1", "--format=%s"]).trim(),
        "Second commit"
    );
    let entries = oplog_entries(&tab, cx);
    assert_eq!(
        entries.first().map(|e| e.title.as_str()),
        Some("Commit \"Second commit\"")
    );
    assert!(
        status_paths(&tab, cx, true).is_empty(),
        "the status was reloaded"
    );
    assert!(
        cx.update(|cx| toast_texts(&app, cx)).is_empty(),
        "a commit has no success toast (07 §12.5)"
    );
}
