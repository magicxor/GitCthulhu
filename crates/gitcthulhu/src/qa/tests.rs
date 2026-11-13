use core::prelude::v1::test;

use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

use super::verbs::{Action, StepResult};
use super::{Args, QaRun, args::QaArgs, dsl, gate::Sandbox, init, verbs};
use crate::backend::{ChangeKind, RelaPath};
use crate::settings::{self, DiffViewMode};
use crate::tab::ui::{FileList, Selection};
use crate::test_support::{DEFAULT_SIZE, RepoSeed, mount_seeded};

/// Mounts a seeded tab and installs the QA verbs over a sandbox in the temp directory.
fn mount(
    cx: &mut TestAppContext,
    seed: RepoSeed,
) -> (
    AnyWindowHandle,
    gpui_kit::Entity<crate::tab::RepoTab>,
    tempfile::TempDir,
) {
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    let dir = tempfile::tempdir().expect("sandbox dir");
    let run = QaRun {
        sandbox: Sandbox::for_tests(dir.path().to_path_buf()),
        args: QaArgs::default(),
    };
    cx.update(|cx| init(cx, &run));
    (handle, tab, dir)
}

/// Runs one scenario line (a synchronous verb) in the window.
fn run_line(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    sandbox: &std::path::Path,
    text: &str,
) -> StepResult {
    let lines = dsl::parse(text).expect("a valid line");
    let sandbox = sandbox.to_path_buf();
    cx.update_window(handle, |_, window, cx| {
        let (verb, consumed) =
            verbs::lookup(cx, &lines[0].tokens).unwrap_or_else(|| panic!("no verb for '{text}'"));
        let args = Args::new(lines[0].tokens[consumed..].to_vec(), sandbox);
        match &verb.action {
            Action::Sync(handler) => handler(window, cx, &args),
            Action::Runner(kind) => panic!("{kind:?} is a runner verb"),
        }
    })
    .expect("the test window is open")
}

fn selection(cx: &mut TestAppContext, tab: &gpui_kit::Entity<crate::tab::RepoTab>) -> Selection {
    cx.update(|cx| {
        let ctx = tab.read(cx).ctx().clone();
        ctx.ui.read(cx).selection.clone()
    })
}

#[gpui_kit::test]
fn the_verb_table_has_the_documented_verbs(cx: &mut TestAppContext) {
    let (_handle, _tab, _dir) = mount(cx, RepoSeed::linear(2));
    let listing = cx.update(|cx| verbs::describe(cx));
    for usage in [
        "open repository",
        "select log row",
        "select file",
        "open diff",
        "toggle diff",
        "open context menu",
        "open dialog",
        "open settings page",
        "set theme",
        "resize",
        "wait idle",
        "wait dialog",
        "begin action",
        "shot",
        "assert no error toast",
        "quit",
    ] {
        assert!(
            listing.lines().any(|l| l.starts_with(usage)),
            "verbs.txt lists '{usage}':\n{listing}"
        );
    }
}

#[gpui_kit::test]
fn begin_action_does_not_wait_for_the_app_to_settle(cx: &mut TestAppContext) {
    let (_handle, _tab, _dir) = mount(cx, RepoSeed::linear(1));
    cx.update(|cx| {
        let tokens = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let (begin, words) = verbs::lookup(cx, &tokens("begin action Push")).expect("begin action");
        assert_eq!((begin.name(), words), ("begin action", 2));
        assert!(
            !begin.settle,
            "an operation waiting for a prompt never settles"
        );
        let (action, _) = verbs::lookup(cx, &tokens("action Push")).expect("action");
        assert!(action.settle, "the plain action still waits");
    });
}

#[gpui_kit::test]
fn the_longest_verb_name_wins(cx: &mut TestAppContext) {
    let (_handle, _tab, _dir) = mount(cx, RepoSeed::linear(1));
    cx.update(|cx| {
        let tokens = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let (idle, words) = verbs::lookup(cx, &tokens("wait idle 500")).expect("wait idle");
        assert_eq!(
            (idle.name(), words),
            ("wait idle", 2),
            "wait idle beats wait"
        );
        let (wait, words) = verbs::lookup(cx, &tokens("wait 500")).expect("wait");
        assert_eq!((wait.name(), words), ("wait", 1), "plain wait");
        assert!(
            verbs::lookup(cx, &tokens("frobnicate now")).is_none(),
            "unknown verbs"
        );
    });
}

#[gpui_kit::test]
fn a_region_verb_replaces_nothing_but_adds_to_the_table(cx: &mut TestAppContext) {
    let (handle, _tab, dir) = mount(cx, RepoSeed::linear(1));
    cx.update(|cx| {
        super::register_verb(
            cx,
            super::Verb::new(
                "hello region",
                "hello region <who>",
                "Greets",
                |_, _, args| Ok(format!("hello {}", args.rest(0))),
            ),
        );
    });
    let result = run_line(cx, handle, dir.path(), "hello region world");
    assert_eq!(
        result,
        Ok("hello world".to_owned()),
        "the registered handler ran"
    );
}

#[gpui_kit::test]
fn select_log_row_selects_commits_and_the_uncommitted_row(cx: &mut TestAppContext) {
    let seed = RepoSeed::linear(3).with_changes(&[], &[("a.txt", ChangeKind::Modified)]);
    let (handle, tab, dir) = mount(cx, seed);
    let uncommitted = run_line(cx, handle, dir.path(), "select log row 0");
    assert!(uncommitted.is_ok(), "row 0 exists: {uncommitted:?}");
    assert_eq!(
        selection(cx, &tab),
        Selection::UncommittedChanges,
        "the first row of a dirty repository is the uncommitted row"
    );
    let first = run_line(cx, handle, dir.path(), "select log row 1");
    assert!(first.is_ok(), "row 1 exists: {first:?}");
    assert!(
        matches!(selection(cx, &tab), Selection::Commit { .. }),
        "row 1 is the newest commit"
    );
    let missing = run_line(cx, handle, dir.path(), "select log row 99").expect_err("no such row");
    assert!(missing.contains("99"), "the error names the row: {missing}");
}

#[gpui_kit::test]
fn select_file_and_open_diff_use_the_status_lists(cx: &mut TestAppContext) {
    let seed = RepoSeed::linear(2).with_changes(
        &[("staged.txt", ChangeKind::Added)],
        &[("src/main.rs", ChangeKind::Modified)],
    );
    let (handle, tab, dir) = mount(cx, seed);
    let ok = run_line(cx, handle, dir.path(), "select file unstaged src/main.rs");
    assert!(ok.is_ok(), "the file is in the list: {ok:?}");
    let single = cx.update(|cx| {
        let ctx = tab.read(cx).ctx().clone();
        ctx.ui
            .read(cx)
            .files
            .single(FileList::Unstaged)
            .map(ToOwned::to_owned)
    });
    assert_eq!(
        single,
        Some(RelaPath::from("src/main.rs")),
        "one unstaged file is selected"
    );

    let typo = run_line(cx, handle, dir.path(), "select file staged nope.txt").expect_err("typo");
    assert!(
        typo.contains("nope.txt"),
        "the error names the path: {typo}"
    );

    let cleared = run_line(cx, handle, dir.path(), "clear file selection");
    assert!(cleared.is_ok(), "clearing works: {cleared:?}");
    let opened = run_line(cx, handle, dir.path(), "open diff");
    assert_eq!(
        opened,
        Ok("Unstaged src/main.rs".to_owned()),
        "open diff without arguments picks the first unstaged file"
    );
}

#[gpui_kit::test]
fn open_repository_refuses_paths_outside_the_sandbox(cx: &mut TestAppContext) {
    let (handle, _tab, dir) = mount(cx, RepoSeed::linear(1));
    let elsewhere = tempfile::tempdir().expect("another dir");
    let line = format!("open repository \"{}\"", elsewhere.path().display());
    let err = run_line(cx, handle, dir.path(), &line).expect_err("outside the sandbox");
    assert!(
        err.contains("outside the QA sandbox"),
        "the gate speaks: {err}"
    );
}

#[gpui_kit::test]
fn settings_verbs_change_the_settings(cx: &mut TestAppContext) {
    let (handle, _tab, dir) = mount(cx, RepoSeed::linear(1));
    assert!(
        run_line(cx, handle, dir.path(), "set diff split").is_ok(),
        "set diff"
    );
    assert_eq!(
        cx.update(|cx| settings::get_or_default(cx).diff_view),
        DiffViewMode::Split,
        "split mode is set"
    );
    assert!(
        run_line(cx, handle, dir.path(), "toggle diff").is_ok(),
        "toggle diff"
    );
    assert_eq!(
        cx.update(|cx| settings::get_or_default(cx).diff_view),
        DiffViewMode::Unified,
        "toggling flips it back"
    );
    assert!(
        run_line(cx, handle, dir.path(), "set theme light").is_ok(),
        "set theme"
    );
    assert!(
        cx.update(|cx| settings::get_or_default(cx).theme == crate::theme::ThemeKind::Light),
        "the light theme is requested"
    );
    assert!(
        run_line(cx, handle, dir.path(), "set theme neon").is_err(),
        "unknown theme"
    );
}

#[gpui_kit::test]
fn context_menus_without_items_fail_instead_of_opening_nothing(cx: &mut TestAppContext) {
    let (handle, _tab, dir) = mount(cx, RepoSeed::branchy());
    let err = run_line(cx, handle, dir.path(), "open context menu log-row 1");
    // Regions fill the menus later; until then the step reports it, afterwards it opens the menu.
    match err {
        Ok(detail) => assert!(detail.contains("items"), "a menu opened: {detail}"),
        Err(message) => assert!(message.contains("no items"), "{message}"),
    }
    let bad =
        run_line(cx, handle, dir.path(), "open context menu nonsense").expect_err("unknown target");
    assert!(bad.contains("nonsense"), "the target is named: {bad}");
}

#[gpui_kit::test]
fn assertions_look_at_the_state(cx: &mut TestAppContext) {
    let (handle, _tab, dir) = mount(cx, RepoSeed::linear(1));
    assert!(
        run_line(cx, handle, dir.path(), "assert tab count 1").is_ok(),
        "one tab"
    );
    assert!(
        run_line(cx, handle, dir.path(), "assert tab count 2").is_err(),
        "not two"
    );
    assert!(
        run_line(cx, handle, dir.path(), "assert dialog closed").is_ok(),
        "no dialog"
    );
    assert!(
        run_line(cx, handle, dir.path(), "assert no error toast").is_ok(),
        "no toast yet"
    );
    cx.update(|cx| {
        crate::app_view::show_toast(
            cx,
            None,
            crate::notifications::ToastKind::Error,
            "boom",
            None,
        );
    });
    cx.run_until_parked();
    let err = run_line(cx, handle, dir.path(), "assert no error toast")
        .expect_err("an error toast was shown");
    assert!(err.contains("boom"), "the toast text is reported: {err}");
    assert!(
        run_line(cx, handle, dir.path(), "assert toast boom").is_ok(),
        "the toast is remembered"
    );
}
