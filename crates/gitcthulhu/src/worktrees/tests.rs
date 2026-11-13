//! Headless tests of the worktree feature (spec 21 §5.7, §8.2, §8.3): the section after Submodules and its rows
//! against the layout numbers, the menus, the Add worktree dialog, the operations against real git (lock, remove with
//! force retry, prune with undo-log clean-up), opening a worktree as its own tab and the "checked out in another
//! worktree" explanation.

use core::prelude::v1::test;

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, Context, Entity, TestAppContext, Window};

use super::{branch_label, ids};
use crate::app_view::AppView;
use crate::backend::present::OpFailure;
use crate::backend::{
    GitError, HeadState, OpOutcome, RefreshSet, StartPoint, WorktreeAddOptions, WorktreeInfo,
    WorktreeTarget,
};
use crate::dialogs::worktree as dialog;
use crate::menus::{self, MenuItem, MenuKind, MenuSlot, MenuTarget};
use crate::side_panel::ids as side_ids;
use crate::side_panel::model::Section;
use crate::tab::ops::OpKind;
use crate::tab::ui::RepoUiEvent;
use crate::tab::{RepoTab, TabCtx};
use crate::test_support::geometry::{assert_height, assert_width, bounds};
use crate::test_support::seed::{full, oid};
use crate::test_support::{DEFAULT_SIZE, Fixture, RepoSeed, TestCall, mount_fixture, mount_seeded};

/// The trigger buttons of the dialog's dropdowns (`<id>-button`).
const BRANCH_DD: &str = "worktree-branch-button";
const START_DD: &str = "worktree-start-button";

// ---- helpers ----------------------------------------------------------------------------------------------------

fn frame(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

fn with_window<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let result = cx
        .update_window(handle, |_, window, cx| f(window, cx))
        .expect("window");
    cx.run_until_parked();
    result
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    });
    frame(cx, handle);
}

fn present(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    id: impl Into<gpui_kit::ElementId>,
) -> bool {
    let id = id.into();
    with_window(cx, handle, |window, _| window.try_find(id).is_some())
}

fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str, text: &str) {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
        window.render_frame(cx);
        window.input(text, cx);
    });
    frame(cx, handle);
}

fn ctx_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> TabCtx {
    cx.update(|cx| tab.read(cx).ctx().clone())
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

fn calls(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<TestCall> {
    cx.update(|cx| crate::test_support::calls(tab, cx))
}

fn script_op<V: Send + 'static>(
    tab: &Entity<RepoTab>,
    cx: &mut TestAppContext,
    result: Result<V, OpFailure>,
    delay: std::time::Duration,
) {
    cx.update(|cx| crate::test_support::script_op(tab, cx, result, delay));
}

fn toasts_of(app: &Entity<AppView>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| crate::test_support::toast_texts(app, cx))
}

fn outcome<T>(value: T) -> OpOutcome<T> {
    OpOutcome {
        value,
        refresh: RefreshSet::empty(),
        log: Default::default(),
        warnings: Vec::new(),
    }
}

fn expand(tab: &Entity<RepoTab>, cx: &mut TestAppContext) {
    let ctx = ctx_of(tab, cx);
    cx.update(|cx| {
        ctx.ui.update(cx, |ui, cx| {
            ui.side_expanded.worktrees = true;
            cx.emit(RepoUiEvent::TogglesChanged);
            cx.notify();
        })
    });
}

fn scratch() -> PathBuf {
    std::env::temp_dir().join("gitcthulhu-x6-tests")
}

fn info(id: Option<&str>, name: &str, head: Option<HeadState>) -> WorktreeInfo {
    let path = scratch().join(name);
    WorktreeInfo {
        id: id.map(str::to_owned),
        git_dir: path.join(".git"),
        path,
        head,
        is_current: false,
        locked: None,
        prunable: false,
        bare: false,
    }
}

fn on_branch(name: &str) -> Option<HeadState> {
    Some(HeadState::Branch {
        name: full(&format!("refs/heads/{name}")),
        target: oid(1),
    })
}

/// A main worktree (this tab), a branch worktree, a locked one, a detached one and one whose directory is gone.
fn worktree_seed() -> RepoSeed {
    let mut seed = RepoSeed::linear(3);
    let mut main = info(None, "main-repo", on_branch("main"));
    main.is_current = true;
    let feature = info(Some("feature-a"), "feature-a", on_branch("feature/a"));
    let mut locked = info(Some("locked"), "locked", on_branch("release"));
    locked.locked = Some("on a usb stick".into());
    let detached = info(
        Some("detached"),
        "detached",
        Some(HeadState::Detached { target: oid(2) }),
    );
    let mut gone = info(Some("gone"), "gone", on_branch("old"));
    gone.prunable = true;
    seed.worktrees = vec![main, feature, locked, detached, gone];
    seed.submodules = vec![crate::backend::SubmoduleInfo {
        name: "vendor".into(),
        path: "vendor".into(),
        url: None,
        branch: None,
        state: crate::backend::SubmoduleState::Initialized,
        index_commit: None,
        head_commit: None,
        dirty: None,
    }];
    seed
}

fn item_ids(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            MenuItem::Action { id, .. } => id.to_string(),
            MenuItem::Separator => "---".to_owned(),
        })
        .collect()
}

fn enabled(items: &[MenuItem], wanted: &str) -> Option<bool> {
    items.iter().find_map(|i| match i {
        MenuItem::Action { id, enabled, .. } if id.as_ref() == wanted => Some(*enabled),
        _ => None,
    })
}

fn menu_for(tab: &Entity<RepoTab>, name: &str, cx: &mut TestAppContext) -> Vec<MenuItem> {
    let ctx = ctx_of(tab, cx);
    cx.update(|cx| {
        let list = ctx.model.read(cx).worktrees.value.clone().expect("loaded");
        let info = list
            .iter()
            .find(|w| super::display_name(&w.path) == name)
            .expect("the worktree is listed")
            .clone();
        let target = crate::menus::worktree::row_target(&ctx, &info);
        crate::menus::worktree::worktree_row(&target, cx)
    })
}

fn git_in(dir: &Path, fx: &Fixture, args: &[&str]) -> String {
    let mut full = vec!["-C", dir.to_str().expect("utf-8 path")];
    full.extend_from_slice(args);
    fx.git(&full)
}

/// A fixture repository with one commit and a linked worktree `wt` (branch `side`) next to it.
fn fixture_with_worktree() -> (Fixture, PathBuf) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let wt = fx.dir.path().join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "side",
        wt.to_str().expect("utf-8 path"),
    ]);
    (fx, wt)
}

fn worktree_paths(fx: &Fixture) -> Vec<String> {
    fx.git(&["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|l| l.strip_prefix("worktree ").map(str::to_owned))
        .collect()
}

fn model_worktrees(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<WorktreeInfo> {
    let ctx = ctx_of(tab, cx);
    cx.update(|cx| {
        ctx.model
            .read(cx)
            .worktrees
            .value
            .as_ref()
            .map(|l| l.to_vec())
            .unwrap_or_default()
    })
}

// ---- section and rows -------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn section_after_submodules_collapsed(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let seeded = model_worktrees(&tab, cx);
    assert_eq!(seeded.len(), 5);
    with_window(cx, handle, |window, _| {
        let subs = bounds(window, side_ids::header(Section::Submodules));
        let wts = bounds(window, side_ids::header(Section::Worktrees));
        assert!(
            wts.origin.y > subs.origin.y,
            "the Worktrees header comes after the Submodules header"
        );
        // One submodule row (36 dp) sits between the two headers.
        assert!(
            (wts.origin.y - subs.origin.y).as_f32() >= 2. * 36. - 0.5,
            "the Submodules rows come before the Worktrees header"
        );
        assert_height(window, side_ids::header(Section::Worktrees), 36.);
        assert_width(
            window,
            side_ids::header(Section::Worktrees),
            bounds(window, side_ids::SIDE_PANEL).size.width.as_f32(),
        );
        assert!(
            window.try_find(ids::row(&seeded[1].path)).is_none(),
            "collapsed by default: no worktree rows"
        );
    });
    // The header opens the section.
    click(cx, handle, side_ids::header(Section::Worktrees));
    assert!(present(cx, handle, ids::row(&seeded[1].path)));
    with_window(cx, handle, |window, _| {
        let header = bounds(window, side_ids::header(Section::Worktrees));
        let first = bounds(window, ids::row(&seeded[0].path));
        assert!(
            (first.origin.y - header.origin.y - header.size.height)
                .as_f32()
                .abs()
                <= 0.5,
            "the first row follows the header"
        );
    });
    click(cx, handle, side_ids::header(Section::Worktrees));
    assert!(
        !present(cx, handle, ids::row(&seeded[1].path)),
        "collapsed again"
    );
}

#[gpui_kit::test]
fn rows_branch_lock_current(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    expand(&tab, cx);
    frame(cx, handle);
    let list = model_worktrees(&tab, cx);
    with_window(cx, handle, |window, _| {
        for w in &list {
            let row = ids::row(&w.path);
            let rb = bounds(window, row.clone());
            assert_height(window, row.clone(), 36.);
            // 07 §6.4: icon 16 dp at x = 32, text at x = 56.
            let icon = bounds(window, ids::part(row.clone(), "icon"));
            assert!(
                ((icon.origin.x - rb.origin.x).as_f32() - 32.).abs() <= 0.5,
                "icon at 32"
            );
            assert!((icon.size.width.as_f32() - 16.).abs() <= 0.5, "16 dp icon");
            let text = bounds(window, ids::part(row.clone(), "text"));
            assert!(
                ((text.origin.x - rb.origin.x).as_f32() - 56.).abs() <= 0.5,
                "text at 56"
            );
            // 21 §5.7 / 07 §6.5: the trailing content has 16 dp horizontal padding, so the branch label ends 16 dp
            // before the row's end.
            let branch = bounds(window, ids::part(row.clone(), "branch"));
            let end = (rb.origin.x + rb.size.width) - (branch.origin.x + branch.size.width);
            assert!(
                (end.as_f32() - 16.).abs() <= 0.5,
                "the branch label ends 16 dp before the row's end, got {end:?}"
            );
            assert!(branch.size.width.as_f32() > 0., "the label has text");
            assert_eq!(
                window.try_find(ids::part(row.clone(), "lock")).is_some(),
                w.locked.is_some(),
                "lock marker iff locked ({})",
                w.path.display()
            );
            assert_eq!(
                window
                    .try_find(ids::part(row.clone(), "prunable"))
                    .is_some(),
                w.prunable,
                "prunable marker iff prunable ({})",
                w.path.display()
            );
        }
        let locked = ids::row(&list[2].path);
        let lock = bounds(window, ids::part(locked.clone(), "lock"));
        let branch = bounds(window, ids::part(locked, "branch"));
        assert!(
            (lock.size.width.as_f32() - 14.).abs() <= 0.5,
            "the lock icon is 14 dp"
        );
        assert!(
            ((branch.origin.x - (lock.origin.x + lock.size.width)).as_f32() - 4.).abs() <= 0.5,
            "the lock precedes the branch with a 4 dp gap"
        );
        let gone = ids::row(&list[4].path);
        let warn = bounds(window, ids::part(gone.clone(), "prunable"));
        assert!(
            (warn.size.width.as_f32() - 14.).abs() <= 0.5,
            "the prunable marker is 14 dp"
        );
        assert!(
            warn.origin.x + warn.size.width <= bounds(window, ids::part(gone, "branch")).origin.x,
            "the prunable marker precedes the branch"
        );
    });
    assert_eq!(branch_label(&list[0]).as_ref(), "main");
    assert_eq!(branch_label(&list[1]).as_ref(), "feature/a");
    assert_eq!(branch_label(&list[3]).as_ref(), "detached");
    let mut bare = list[0].clone();
    bare.bare = true;
    assert_eq!(branch_label(&bare).as_ref(), "bare");
    bare.bare = false;
    bare.head = None;
    assert_eq!(branch_label(&bare).as_ref(), "unknown");
}

#[gpui_kit::test]
fn current_row_is_distinguished_by_the_open_folder(cx: &mut TestAppContext) {
    // The current marker is the open-folder icon plus bold text; both are properties of the element tree, so the
    // test compares what is observable: the icon and text boxes exist for the current and for other rows, and the
    // current worktree is the one with `is_current`.
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    expand(&tab, cx);
    frame(cx, handle);
    let list = model_worktrees(&tab, cx);
    assert!(list[0].is_current && !list[1].is_current);
    assert!(present(
        cx,
        handle,
        ids::part(ids::row(&list[0].path), "icon")
    ));
}

// ---- menus ---------------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn row_menu_items_per_kind_of_worktree(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    let main = menu_for(&tab, "main-repo", cx);
    assert_eq!(
        item_ids(&main),
        ["worktree-open", "worktree-reveal", "worktree-copy-path"],
        "the main worktree: no lock, no remove"
    );
    assert_eq!(
        enabled(&main, "worktree-open"),
        Some(false),
        "the open tab is not opened twice"
    );

    let linked = menu_for(&tab, "feature-a", cx);
    assert_eq!(
        item_ids(&linked),
        [
            "worktree-open",
            "worktree-reveal",
            "worktree-copy-path",
            "---",
            "worktree-lock",
            "worktree-remove"
        ]
    );
    assert_eq!(enabled(&linked, "worktree-open"), Some(true));

    let locked = menu_for(&tab, "locked", cx);
    let ids = item_ids(&locked);
    assert!(
        ids.contains(&"worktree-unlock".to_owned()) && !ids.contains(&"worktree-lock".to_owned())
    );

    let gone = menu_for(&tab, "gone", cx);
    assert_eq!(
        enabled(&gone, "worktree-open"),
        Some(false),
        "a missing directory cannot be opened"
    );
    assert_eq!(enabled(&gone, "worktree-reveal"), Some(false));
    assert_eq!(
        enabled(&gone, "worktree-remove"),
        Some(true),
        "git can still remove the entry"
    );
}

#[gpui_kit::test]
fn header_menu_has_add_and_prune(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    with_window(cx, handle, |window, cx| {
        window.right_click(side_ids::header(Section::Worktrees), cx);
    });
    frame(cx, handle);
    assert!(present(cx, handle, "menu-item-worktrees-add"));
    assert!(present(cx, handle, "menu-item-worktrees-prune"));
    click(cx, handle, "menu-item-worktrees-prune");
    assert!(
        calls(&tab, cx).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: OpKind::PruneWorktrees,
                ..
            }
        )),
        "Prune worktrees runs the operation: {:?}",
        calls(&tab, cx)
    );
}

#[gpui_kit::test]
fn right_click_on_a_row_opens_its_menu(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    expand(&tab, cx);
    frame(cx, handle);
    let list = model_worktrees(&tab, cx);
    with_window(cx, handle, |window, cx| {
        window.right_click(ids::row(&list[1].path), cx);
    });
    frame(cx, handle);
    assert!(present(cx, handle, "menu-item-worktree-lock"));
    assert!(present(cx, handle, "menu-item-worktree-remove"));
    click(cx, handle, "menu-item-worktree-copy-path");
    let copied = crate::menus::refs::tests_support::COPIED.with(|c| c.borrow().last().cloned());
    assert_eq!(copied, Some(list[1].path.to_string_lossy().into_owned()));
}

#[gpui_kit::test]
fn branch_menu_create_worktree(cx: &mut TestAppContext) {
    let mut seed = worktree_seed();
    let mut other = crate::backend::LocalBranch {
        name: "topic".into(),
        full: full("refs/heads/topic"),
        target: oid(5),
        is_head: false,
        upstream: None,
        ahead_behind: None,
        checked_out_in: None,
    };
    seed.refs.local.push(other.clone());
    other.name = "busy".into();
    other.full = full("refs/heads/busy");
    other.checked_out_in = Some(scratch().join("somewhere"));
    seed.refs.local.push(other);
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle);
    let ctx = ctx_of(&tab, cx);
    let items_of = |cx: &mut TestAppContext, name: &str| -> Vec<MenuItem> {
        let ctx = ctx.clone();
        let name = name.to_owned();
        cx.update(|cx| {
            let branch = ctx
                .model
                .read(cx)
                .refs
                .value
                .as_ref()
                .expect("refs")
                .local
                .iter()
                .find(|b| b.name == name)
                .expect("branch")
                .clone();
            let target = crate::menus::refs::local_branch_target(&ctx, &branch);
            crate::menus::refs::local_branch(&target, cx)
        })
    };
    let topic = items_of(cx, "topic");
    let ids = item_ids(&topic);
    let checkout = ids
        .iter()
        .position(|i| i == "branch-checkout")
        .expect("checkout");
    assert_eq!(
        ids[checkout + 1],
        "branch-create-worktree",
        "Create worktree… follows Checkout branch (AfterCheckout): {ids:?}"
    );
    assert!(
        !item_ids(&items_of(cx, "busy")).contains(&"branch-create-worktree".to_owned()),
        "not for a branch checked out elsewhere"
    );
    let current = RepoSeed::linear(1)
        .refs
        .local
        .iter()
        .find(|b| b.is_head)
        .map(|b| b.name.clone());
    let head_name = current.unwrap_or_else(|| "main".into());
    assert!(
        !item_ids(&items_of(cx, &head_name)).contains(&"branch-create-worktree".to_owned()),
        "not for the current branch"
    );

    // Choosing it opens the dialog on that branch.
    with_window(cx, handle, |window, cx| {
        menus::open_context_menu(
            window,
            cx,
            gpui_kit::point(gpui_kit::px(10.), gpui_kit::px(10.)),
            topic.clone(),
        );
    });
    frame(cx, handle);
    click(cx, handle, "menu-item-branch-create-worktree");
    assert!(present(cx, handle, dialog::ids::DIALOG));
    assert!(
        present(cx, handle, BRANCH_DD),
        "the existing-branch mode with the branch preselected"
    );
}

// ---- operations on scripted outcomes --------------------------------------------------------------------------

#[gpui_kit::test]
fn remove_force_retry(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let target = scratch().join("feature-a");
    script_op(
        &tab,
        cx,
        Err::<OpOutcome<String>, _>(OpFailure::Git(std::sync::Arc::new(
            GitError::UncommittedChanges {
                stderr: "contains modified or untracked files".into(),
            },
        ))),
        std::time::Duration::ZERO,
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(target.clone(), false, w, c)
    });
    frame(cx, handle);
    assert!(
        present(cx, handle, "dialog-primary"),
        "the confirmation 'remove it anyway' is open"
    );
    let before = calls(&tab, cx).len();
    script_op(
        &tab,
        cx,
        Ok(outcome(String::from("feature-a"))),
        std::time::Duration::ZERO,
    );
    click(cx, handle, "dialog-primary");
    let after = calls(&tab, cx);
    assert!(
        after.len() > before
            && matches!(
                after.last(),
                Some(TestCall::Op {
                    kind: OpKind::RemoveWorktree,
                    ..
                })
            ),
        "confirming retries the removal: {after:?}"
    );
}

#[gpui_kit::test]
fn remove_of_this_tabs_worktree_explains(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let path = scratch().join("main-repo");
    script_op(
        &tab,
        cx,
        Err::<OpOutcome<String>, _>(OpFailure::Git(std::sync::Arc::new(
            GitError::CurrentWorktree,
        ))),
        std::time::Duration::ZERO,
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(path.clone(), false, w, c)
    });
    frame(cx, handle);
    let texts = cx.update(|cx| app.read(cx).error_texts.clone());
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Close the tab of this worktree first")),
        "{texts:?}"
    );
}

/// The sentence comes from the kind of the error: a tab model that does not know the worktree (or knows it
/// wrongly) cannot pick the wrong one.
#[gpui_kit::test]
fn remove_refusal_text_follows_the_error_not_the_model(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    // The model flags `main-repo` as the current worktree; Git says the main worktree cannot be removed.
    let main = scratch().join("main-repo");
    script_op(
        &tab,
        cx,
        Err::<OpOutcome<String>, _>(OpFailure::Git(std::sync::Arc::new(GitError::MainWorktree))),
        std::time::Duration::ZERO,
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(main.clone(), false, w, c)
    });
    frame(cx, handle);
    let texts = cx.update(|cx| app.read(cx).error_texts.clone());
    assert!(
        texts
            .iter()
            .any(|t| t.contains("The main worktree cannot be removed")),
        "{texts:?}"
    );
    frame(cx, handle);
    with_window(cx, handle, |window, cx| window.close_dialog(cx));
    // `feature-a` is not current in the model; Git says it is this tab's worktree.
    let feature = scratch().join("feature-a");
    script_op(
        &tab,
        cx,
        Err::<OpOutcome<String>, _>(OpFailure::Git(std::sync::Arc::new(
            GitError::CurrentWorktree,
        ))),
        std::time::Duration::ZERO,
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(feature.clone(), false, w, c)
    });
    frame(cx, handle);
    let texts = cx.update(|cx| app.read(cx).error_texts.clone());
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Close the tab of this worktree first")),
        "{texts:?}"
    );
}

fn message_info(head: Option<HeadState>, locked: Option<&str>) -> WorktreeInfo {
    let mut wt = info(Some("wt"), "wt", head);
    wt.locked = locked.map(str::to_owned);
    wt
}

#[test]
fn removal_message_keeps_the_branch_claim_for_branches_only() {
    use crate::menus::worktree::removal_message;
    let branch = message_info(on_branch("feature/a"), None);
    assert_eq!(
        removal_message(&branch, None),
        "Remove the worktree 'wt'? Its directory is deleted; its branch is kept."
    );
    let detached = message_info(Some(HeadState::Detached { target: oid(2) }), None);
    let short = oid(2).to_hex_with_len(7).to_string();
    let reachable = removal_message(&detached, Some(true));
    assert_eq!(
        reachable,
        format!("Remove the worktree 'wt'? Its directory is deleted. HEAD is detached at {short}."),
        "a detached HEAD on a ref says where it is and never claims a branch"
    );
    let lost = removal_message(&detached, Some(false));
    assert_eq!(
        lost,
        format!(
            "Remove the worktree 'wt'? Its directory is deleted. HEAD is detached at {short} and its commits are on no branch or tag; they will be lost."
        )
    );
    assert!(
        removal_message(&detached, None).contains("was not checked"),
        "a failed check is not reported as safe"
    );
    let locked = message_info(Some(HeadState::Detached { target: oid(2) }), Some("usb"));
    assert!(
        removal_message(&locked, Some(false)).starts_with("The worktree 'wt' is locked (usb).")
            && removal_message(&locked, Some(false)).contains("they will be lost."),
        "a locked worktree warns as well"
    );
}

/// The reachability check runs on the real repository off the UI thread: a commit made in a detached worktree is
/// on no ref until a branch is created for it.
#[gpui_kit::test]
fn remove_of_a_detached_worktree_warns_when_its_commits_are_on_no_ref(cx: &mut TestAppContext) {
    let (fx, _) = fixture_with_worktree();
    let wt = fx.dir.path().join("wt-detached");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        wt.to_str().expect("utf-8 path"),
    ]);
    git_in(
        &wt,
        &fx,
        &["commit", "-q", "--allow-empty", "-m", "only here"],
    );
    let on_ref = fx.dir.path().join("wt-on-ref");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        on_ref.to_str().expect("utf-8 path"),
    ]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    let listed = model_worktrees(&tab, &mut *cx);
    let find = |dir: &Path| {
        listed
            .iter()
            .find(|w| {
                crate::tab::ops::worktree::display_name(&w.path)
                    == dir.file_name().unwrap().to_string_lossy()
            })
            .cloned()
            .expect("the worktree is listed")
    };
    crate::menus::worktree::tests_support::MESSAGES.with(|m| m.borrow_mut().clear());
    for dir in [&wt, &on_ref] {
        let info = find(dir);
        with_window(cx, handle, |window, cx| {
            crate::menus::worktree::remove_worktree(window, cx, &ctx, &info)
        });
        for _ in 0..3 {
            cx.run_until_parked();
        }
        with_window(cx, handle, |window, cx| window.close_dialog(cx));
    }
    let messages = crate::menus::worktree::tests_support::MESSAGES.with(|m| m.borrow().clone());
    assert_eq!(messages.len(), 2, "both confirmations opened: {messages:?}");
    assert!(
        messages[0].contains("on no branch or tag; they will be lost"),
        "{messages:?}"
    );
    assert!(
        !messages[1].contains("lost") && messages[1].contains("HEAD is detached at"),
        "a detached HEAD on a branch tip is safe: {messages:?}"
    );
}

#[gpui_kit::test]
fn prune_toast_says_when_nothing_was_pruned(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    script_op(
        &tab,
        cx,
        Ok(outcome(Vec::<String>::new())),
        std::time::Duration::ZERO,
    );
    run_on(cx, handle, &tab, |t, w, c| t.prune_worktrees(w, c));
    frame(cx, handle);
    let toasts = toasts_of(&app, cx);
    assert!(
        toasts.iter().any(|t| t == "No worktrees to prune"),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn add_success_toast_offers_to_open_the_tab(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, worktree_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let made = info(Some("fresh"), "fresh", on_branch("fresh"));
    script_op(&tab, cx, Ok(outcome(made)), std::time::Duration::ZERO);
    let opts = WorktreeAddOptions {
        path: scratch().join("fresh"),
        target: WorktreeTarget::NewBranch {
            name: "fresh".into(),
            start: StartPoint::Head,
        },
        lock_reason: None,
    };
    run_on(cx, handle, &tab, |t, w, c| t.add_worktree(opts, w, c));
    frame(cx, handle);
    let toasts = toasts_of(&app, cx);
    assert!(
        toasts.iter().any(|t| t == "Worktree 'fresh' added"),
        "{toasts:?}"
    );
}

// ---- the Add worktree dialog -------------------------------------------------------------------------------------

fn dialog_seed() -> RepoSeed {
    let mut seed = worktree_seed();
    // Branches: `main` is HEAD (this tab's worktree), `feature/a`, `release` are checked out in worktrees.
    let branch = |name: &str, head: bool| crate::backend::LocalBranch {
        name: name.to_owned(),
        full: full(&format!("refs/heads/{name}")),
        target: oid(3),
        is_head: head,
        upstream: None,
        ahead_behind: None,
        checked_out_in: None,
    };
    seed.refs.local = vec![
        branch("feature/a", false),
        branch("free-one", false),
        branch("free-two", false),
        branch("main", true),
        branch("release", false),
    ];
    seed
}

#[gpui_kit::test]
fn add_dialog_targets(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, dialog_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        dialog::open_add_worktree(window, cx, ctx.clone())
    });
    frame(cx, handle);
    with_window(cx, handle, |window, _| {
        // 21 §5.7: DialogFrame 560 dp, content 528.
        assert_width(window, crate::ids::DIALOG_FRAME, 560.);
        assert_width(window, dialog::ids::DIALOG, 528.);
        assert_width(window, dialog::ids::PATH, 528.);
        // The Browse button sits inside the end of the path field.
        let field = bounds(window, dialog::ids::PATH);
        let browse = bounds(window, dialog::ids::PATH_BROWSE);
        assert!(
            browse.origin.x > field.origin.x + field.size.width / 2.
                && browse.origin.x + browse.size.width <= field.origin.x + field.size.width
                && browse.origin.y >= field.origin.y - gpui_kit::px(6.)
                && browse.origin.y + browse.size.height
                    <= field.origin.y + field.size.height + gpui_kit::px(6.),
            "Browse overlays the field's end: field {field:?}, browse {browse:?}"
        );
        // Three target cards share the width with 8 dp gaps.
        for id in [
            dialog::ids::MODE_EXISTING,
            dialog::ids::MODE_NEW,
            dialog::ids::MODE_DETACHED,
        ] {
            assert_width(window, id, (528. - 16.) / 3.);
        }
        assert!(
            window.try_find(BRANCH_DD).is_some(),
            "existing branch is the default"
        );
        assert!(window.try_find(dialog::ids::NEW_NAME).is_none());
        assert!(window.try_find(dialog::ids::COMMIT).is_none());
        assert_width(window, BRANCH_DD, 528.);
    });
    // The default path sits next to the repository: <parent>/<repo>-<branch>.
    // Only branches that no worktree has checked out are offered.
    click(cx, handle, dialog::ids::MODE_NEW);
    assert!(present(cx, handle, dialog::ids::NEW_NAME));
    assert!(present(cx, handle, START_DD));
    assert!(!present(cx, handle, BRANCH_DD));
    click(cx, handle, dialog::ids::MODE_DETACHED);
    assert!(present(cx, handle, dialog::ids::COMMIT));
    assert!(!present(cx, handle, dialog::ids::NEW_NAME));
    click(cx, handle, dialog::ids::LOCK);
    assert!(
        present(cx, handle, dialog::ids::LOCK_REASON),
        "the lock reason shows once Lock is checked"
    );
    click(cx, handle, dialog::ids::MODE_EXISTING);
    click(cx, handle, "dialog-cancel");
    assert!(!with_window(cx, handle, |window, cx| window.has_active_dialog(cx)));
}

#[gpui_kit::test]
fn add_dialog_offers_only_free_branches(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, dialog_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        dialog::open_add_worktree(window, cx, ctx.clone())
    });
    frame(cx, handle);
    click(cx, handle, "worktree-branch-button");
    for (ix, expected) in ["free-one", "free-two"].iter().enumerate() {
        assert!(
            present(cx, handle, format!("worktree-branch-option-{ix}")),
            "option {ix} ({expected}) is listed"
        );
    }
    assert!(
        !present(cx, handle, "worktree-branch-option-2"),
        "branches of other worktrees, HEAD and the 'release' branch are not listed"
    );
}

#[gpui_kit::test]
fn add_dialog_new_branch_runs_the_operation(cx: &mut TestAppContext) {
    let mut seed = dialog_seed();
    // Give the inert tab a main worktree directory below a path that does not exist.
    seed.worktrees[0].path = scratch().join("nowhere").join("repo");
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        dialog::open_add_worktree(window, cx, ctx.clone())
    });
    frame(cx, handle);
    click(cx, handle, dialog::ids::MODE_NEW);
    // Nothing typed yet: the primary button is disabled and nothing runs.
    click(cx, handle, "dialog-primary");
    assert!(
        calls(&tab, cx)
            .iter()
            .all(|c| !matches!(c, TestCall::Op { .. }))
    );
    type_into(cx, handle, dialog::ids::NEW_NAME, "topic/x");
    script_op(
        &tab,
        cx,
        Ok(outcome(info(
            Some("repo-topic-x"),
            "repo-topic-x",
            on_branch("topic/x"),
        ))),
        std::time::Duration::ZERO,
    );
    click(cx, handle, "dialog-primary");
    cx.run_until_parked();
    let ops: Vec<_> = calls(&tab, cx)
        .into_iter()
        .filter(|c| {
            matches!(
                c,
                TestCall::Op {
                    kind: OpKind::AddWorktree,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(ops.len(), 1, "one worktree add was started");
    assert!(
        !with_window(cx, handle, |window, cx| window.has_active_dialog(cx)),
        "the dialog closed"
    );
}

#[gpui_kit::test]
fn add_dialog_creates_a_real_worktree_and_reports_problems(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.git(&["branch", "existing"]);
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        dialog::open_add_worktree(window, cx, ctx.clone())
    });
    frame(cx, handle);

    // A detached commit that does not exist stays in the dialog with a message.
    click(cx, handle, dialog::ids::MODE_DETACHED);
    type_into(cx, handle, dialog::ids::COMMIT, "no-such-revision");
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        cx.run_until_parked();
    }
    frame(cx, handle);
    assert!(
        present(cx, handle, dialog::ids::ERROR),
        "an unknown commit is reported inline"
    );
    assert!(
        with_window(cx, handle, |window, cx| window.has_active_dialog(cx)),
        "the dialog stays open"
    );

    // Existing branch `existing` into the suggested directory next to the repository.
    click(cx, handle, dialog::ids::MODE_EXISTING);
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        cx.run_until_parked();
    }
    frame(cx, handle);
    let paths = worktree_paths(&fx);
    assert_eq!(paths.len(), 2, "the worktree exists: {paths:?}");
    let created = PathBuf::from(&paths[1]);
    assert!(created.join("a.txt").exists(), "the files are checked out");
    assert!(
        created
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "repo-existing"),
        "the default directory is <repo>-<branch> next to the repository: {created:?}"
    );
    assert_eq!(
        git_in(&created, &fx, &["branch", "--show-current"]).trim(),
        "existing"
    );
    let listed = model_worktrees(&tab, cx);
    assert_eq!(listed.len(), 2, "the loader refreshed the list");
    assert!(
        crate::tab::ops::worktree::display_name(&listed[1].path) == "repo-existing"
            && matches!(&listed[1].head, Some(HeadState::Branch { name, .. }) if name == &full("refs/heads/existing"))
    );
    let errors = cx.update(|cx| app.read(cx).errors_opened.clone());
    assert!(errors.is_empty(), "no error dialog: {errors:?}");
}

/// An edit while the background check runs drops the check's result: no stale banner, no worktree of the old form.
#[gpui_kit::test]
fn add_dialog_drops_the_check_of_an_edited_form(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    with_window(cx, handle, |window, cx| {
        dialog::open_add_worktree(window, cx, ctx.clone())
    });
    frame(cx, handle);
    click(cx, handle, dialog::ids::MODE_DETACHED);
    type_into(cx, handle, dialog::ids::COMMIT, "no-such-revision");
    // Accept and edit in one turn: the check has not run yet when the form changes.
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("dialog-primary", cx);
        window.render_frame(cx);
        window.click(dialog::ids::COMMIT, cx);
        window.input("2", cx);
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    frame(cx, handle);
    assert!(
        !present(cx, handle, dialog::ids::ERROR),
        "the result of the check of the old form is dropped"
    );
    assert!(with_window(cx, handle, |window, cx| window.has_active_dialog(cx)));
    // Accepting again checks the current form.
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        cx.run_until_parked();
    }
    frame(cx, handle);
    assert!(
        present(cx, handle, dialog::ids::ERROR),
        "the new check reports"
    );
    assert_eq!(worktree_paths(&fx).len(), 1, "no worktree was added");
}

// ---- real operations -----------------------------------------------------------------------------------------

#[gpui_kit::test]
fn lock_and_unlock_roundtrip(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, c| {
        t.lock_worktree(wt.clone(), Some("travelling".into()), w, c)
    });
    let porcelain = fx.git(&["worktree", "list", "--porcelain"]);
    assert!(porcelain.contains("locked travelling"), "{porcelain}");
    let listed = model_worktrees(&tab, cx);
    assert_eq!(
        listed[1].locked.as_deref(),
        Some("travelling"),
        "the model shows the lock"
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.unlock_worktree(wt.clone(), w, c)
    });
    assert!(
        !fx.git(&["worktree", "list", "--porcelain"])
            .contains("locked")
    );
    assert_eq!(model_worktrees(&tab, cx)[1].locked, None);
}

#[gpui_kit::test]
fn remove_dirty_worktree_asks_then_forces(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    std::fs::write(wt.join("a.txt"), "changed\n").expect("write");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(wt.clone(), false, w, c)
    });
    frame(cx, handle);
    assert!(
        wt.exists(),
        "a dirty worktree is not removed without asking"
    );
    assert!(
        present(cx, handle, "dialog-primary"),
        "the confirmation is open"
    );
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        cx.run_until_parked();
    }
    assert!(!wt.exists(), "confirmed: removed with force");
    assert_eq!(worktree_paths(&fx).len(), 1);
    assert_eq!(model_worktrees(&tab, cx).len(), 1, "the list refreshed");
    assert!(
        fx.git(&["branch", "--list", "side"]).contains("side"),
        "the branch is kept"
    );
}

#[gpui_kit::test]
fn remove_clears_oplog_ref(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    fx.git(&["update-ref", "refs/gitcthulhu/oplog/linked/wt", "HEAD"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, c| {
        t.remove_worktree(wt.clone(), false, w, c)
    });
    assert!(!wt.exists());
    let refs = fx.git(&["for-each-ref", "refs/gitcthulhu/"]);
    assert!(
        !refs.contains("linked/wt"),
        "the removed worktree's undo chain is gone: {refs}"
    );
}

#[gpui_kit::test]
fn prune_removes_vanished_worktrees_and_their_oplog_ref(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    fx.git(&["update-ref", "refs/gitcthulhu/oplog/linked/wt", "HEAD"]);
    std::fs::remove_dir_all(&wt).expect("remove the directory behind git's back");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    assert!(
        model_worktrees(&tab, cx)[1].prunable,
        "the vanished worktree is marked prunable"
    );
    run_on(cx, handle, &tab, |t, w, c| t.prune_worktrees(w, c));
    assert_eq!(worktree_paths(&fx).len(), 1, "the entry is pruned");
    assert_eq!(model_worktrees(&tab, cx).len(), 1);
    let refs = fx.git(&["for-each-ref", "refs/gitcthulhu/"]);
    assert!(
        !refs.contains("linked/wt"),
        "the pruned worktree's undo chain is gone: {refs}"
    );
    frame(cx, handle);
    assert!(toasts_of(&app, cx).iter().any(|t| t == "Pruned 1 worktree"));
}

// ---- opening a worktree as its own tab ------------------------------------------------------------------------

fn tab_paths(app: &Entity<AppView>, cx: &mut TestAppContext) -> Vec<Option<PathBuf>> {
    cx.update(|cx| {
        let view = app.read(cx);
        view.tabs().iter().map(|s| s.path(cx)).collect()
    })
}

#[gpui_kit::test]
fn open_in_new_tab(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    expand(&tab, cx);
    frame(cx, handle);
    assert_eq!(tab_paths(&app, cx).len(), 1);
    // A single click does nothing; a double-click opens the worktree in a new, selected tab.
    let row = ids::row(&model_worktrees(&tab, cx)[1].path);
    with_window(cx, handle, |window, cx| window.click(row.clone(), cx));
    assert_eq!(tab_paths(&app, cx).len(), 1, "click: nothing");
    with_window(cx, handle, |window, cx| {
        window.double_click(row.clone(), cx)
    });
    for _ in 0..5 {
        cx.run_until_parked();
    }
    let tabs = tab_paths(&app, cx);
    assert_eq!(tabs.len(), 2, "a second tab: {tabs:?}");
    assert!(
        super::same_path(tabs[1].as_ref().expect("path"), &wt),
        "the new tab shows the worktree: {tabs:?}"
    );
    assert_eq!(
        cx.update(|cx| app.read(cx).active()),
        1,
        "the new tab is selected"
    );
    // Opening it again selects the tab that shows it.
    with_window(cx, handle, |window, cx| {
        super::open_in_new_tab(window, cx, wt.clone());
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    assert_eq!(tab_paths(&app, cx).len(), 2, "no duplicate tab");
    // The new tab's repository is the linked worktree: its list marks it as the current one.
    let new_tab = cx
        .update(|cx| app.read(cx).tabs()[1].repo().cloned())
        .expect("repo tab");
    let list = model_worktrees(&new_tab, cx);
    assert!(
        list.iter()
            .any(|w| w.is_current && super::same_path(&w.path, &wt))
    );
    assert_eq!(list.len(), 2);
}

/// Which tab shows a directory is decided in the background (canonicalising can block on a slow drive): the call
/// itself neither opens nor selects anything, and removing a worktree that a tab shows is still refused.
#[gpui_kit::test]
fn tab_lookup_of_a_worktree_runs_off_the_ui_thread(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let during = with_window(cx, handle, |window, cx| {
        super::open_in_new_tab(window, cx, wt.clone());
        app.read(cx).tabs().len()
    });
    assert_eq!(during, 1, "nothing is decided inside the call itself");
    assert_eq!(
        tab_paths(&app, cx).len(),
        2,
        "opened once the lookup finished"
    );
    with_window(cx, handle, |window, cx| {
        super::open_in_new_tab(window, cx, wt.clone());
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    assert_eq!(
        tab_paths(&app, cx).len(),
        2,
        "a tab that shows it is selected"
    );

    // The same directory spelled with a `.` segment is still recognised (canonical comparison), so removing the
    // worktree the second tab shows is refused.
    let ctx = ctx_of(&tab, cx);
    let mut info = model_worktrees(&tab, cx)[1].clone();
    info.path = info.path.join(".");
    with_window(cx, handle, |window, cx| {
        crate::menus::worktree::remove_worktree(window, cx, &ctx, &info)
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    frame(cx, handle);
    let texts = cx.update(|cx| app.read(cx).error_texts.clone());
    assert!(
        texts.iter().any(|t| t.contains("is open in a tab")),
        "{texts:?}"
    );
}

#[gpui_kit::test]
fn checked_out_elsewhere_offers_open_tab(cx: &mut TestAppContext) {
    let (fx, wt) = fixture_with_worktree();
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    // `side` is checked out in `wt`: checking it out here is refused by Git; the dialog explains and offers a tab.
    let side = full("refs/heads/side");
    run_on(cx, handle, &tab, |t, w, c| {
        t.checkout_branch(side.clone(), w, c)
    });
    frame(cx, handle);
    let texts = cx.update(|cx| app.read(cx).error_texts.clone());
    assert!(
        texts
            .iter()
            .any(|t| t.contains("checked out in another worktree")),
        "the error explains the refusal: {texts:?}"
    );
    assert!(
        present(cx, handle, crate::dialogs::error::ids::ERROR_EXTRA),
        "the extra button is offered"
    );
    click(cx, handle, crate::dialogs::error::ids::ERROR_EXTRA);
    for _ in 0..5 {
        cx.run_until_parked();
    }
    let tabs = tab_paths(&app, cx);
    assert_eq!(tabs.len(), 2, "the worktree opened in a new tab: {tabs:?}");
    assert!(super::same_path(tabs[1].as_ref().expect("path"), &wt));
}

#[test]
fn open_in_new_tab_action_is_offered() {
    let action = super::open_in_new_tab_action(PathBuf::from("/x")).expect("an action");
    assert_eq!(action.label.as_ref(), "Open worktree in new tab");
}

// ---- unit tests ---------------------------------------------------------------------------------------------------

#[gpui_kit::test]
fn menu_slot_extension_points_are_registered(cx: &mut TestAppContext) {
    // The package registers exactly one extender: Create worktree… at AfterCheckout of the local-branch menu.
    let (_handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    cx.update(|cx| {
        let target = MenuTarget {
            kind: MenuKind::LocalBranch,
            tab: ctx.clone(),
            branch: Some(full("refs/heads/x")),
            upstream: None,
            commit: None,
            path: None,
            is_current: false,
            checked_out_elsewhere: false,
        };
        let mut items = Vec::new();
        menus::extend(cx, MenuSlot::AfterCheckout, &target, &mut items);
        assert_eq!(item_ids(&items), ["branch-create-worktree", "---"]);
        let mut none = Vec::new();
        menus::extend(cx, MenuSlot::End, &target, &mut none);
        assert!(none.is_empty());
    });
}

/// The QA scenario of this package only uses verbs that exist (built-ins and the ones this package registers).
#[cfg(feature = "qa")]
#[gpui_kit::test]
fn scenario_uses_registered_verbs(cx: &mut TestAppContext) {
    let text = include_str!("../../../../tools/qa/scenarios/m2-worktrees.qa");
    let lines = crate::qa::dsl::parse(text).expect("the scenario tokenizes");
    assert!(lines.len() > 30);
    cx.update(|cx| {
        crate::qa::builtin::register_all(cx);
        super::register(cx);
        for line in &lines {
            assert!(
                crate::qa::verbs::lookup(cx, &line.tokens).is_some(),
                "line {}: no verb for '{}'",
                line.number,
                line.text
            );
        }
        let described = crate::qa::verbs::describe(cx);
        for verb in [
            "worktree add",
            "worktree vanish",
            "open worktrees menu",
            "assert worktree count",
        ] {
            assert!(
                described.contains(verb),
                "{verb} is documented in verbs.txt"
            );
        }
    });
}

/// Drives the scenario's own steps (every verb that is not the runner's or the window's) against a real repository:
/// the verbs register, parse their arguments and call the real operations, and every `assert` of the scenario holds.
#[cfg(feature = "qa")]
#[gpui_kit::test]
fn scenario_verbs_drive_real_worktrees(cx: &mut TestAppContext) {
    use crate::qa::verbs::{Action, Args, lookup};

    let fx = Fixture::new();
    fx.write(
        "a.txt", "one
",
    );
    fx.commit_all("init");
    std::fs::create_dir_all(fx.dir.path().join("repos")).expect("repos dir");
    let (handle, app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    cx.update(crate::qa::builtin::register_all);
    let text = include_str!("../../../../tools/qa/scenarios/m2-worktrees.qa");
    let lines = crate::qa::dsl::parse(text).expect("the scenario tokenizes");
    let skipped = ["resize", "set", "open repository", "wait", "shot"];
    let sandbox = fx.dir.path().to_owned();
    let mut ran = 0;
    for line in &lines {
        let first = line.tokens[0].as_str();
        let second = line.tokens.get(1).map_or("", String::as_str);
        if skipped.contains(&first) || (first == "open" && second == "repository") {
            continue;
        }
        let (verb, used) = cx
            .update(|cx| lookup(cx, &line.tokens))
            .expect("a verb for the line");
        let Action::Sync(handler) = verb.action.clone() else {
            continue;
        };
        let args = Args::new(line.tokens[used..].to_vec(), sandbox.clone());
        let result = with_window(cx, handle, |window, cx| handler(window, cx, &args));
        if let Err(message) = result {
            panic!("line {} '{}' failed: {message}", line.number, line.text);
        }
        for _ in 0..6 {
            cx.run_until_parked();
        }
        frame(cx, handle);
        if line.text.starts_with("open context menu branch") {
            assert!(
                present(cx, handle, "menu-item-branch-create-worktree"),
                "line {}: the freed branch's menu offers Create worktree…",
                line.number
            );
        }
        ran += 1;
    }
    assert!(ran > 25, "the scenario's steps ran ({ran})");
    // The end state of the scenario: main, linear-login (open in tab 2) and linear-v1.
    let names: Vec<String> = worktree_paths(&fx)
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"linear-login".to_owned()) && names.contains(&"linear-v1".to_owned()));
    assert_eq!(tab_paths(&app, cx).len(), 2);

    // The QA branch menu is the real one: a branch checked out in another worktree has no Create worktree….
    let tokens: Vec<String> = ["open", "context", "menu", "branch", "feature/login"]
        .iter()
        .map(|t| (*t).to_owned())
        .collect();
    let (verb, used) = cx
        .update(|cx| lookup(cx, &tokens))
        .expect("the open context menu verb");
    let Action::Sync(handler) = verb.action else {
        panic!("open context menu is synchronous");
    };
    let args = Args::new(tokens[used..].to_vec(), sandbox.clone());
    with_window(cx, handle, |window, cx| handler(window, cx, &args)).expect("the branch menu");
    frame(cx, handle);
    assert!(present(cx, handle, "menu-item-branch-checkout"));
    assert!(
        !present(cx, handle, "menu-item-branch-create-worktree"),
        "feature/login is checked out in linear-login"
    );
    cx.update(crate::menus::close_menu);

    // Scenarios never write outside the sandbox: a worktree directory elsewhere (or relative) is refused.
    let outside = std::env::temp_dir().join("gitcthulhu-x6-outside-the-sandbox");
    for path in [
        outside.display().to_string(),
        "relative-dir".to_owned(),
        format!("{}/../escaped", sandbox.display()),
    ] {
        let tokens: Vec<String> = ["worktree", "add", path.as_str(), "new", "escape"]
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        let (verb, used) = cx
            .update(|cx| lookup(cx, &tokens))
            .expect("the worktree add verb");
        let Action::Sync(handler) = verb.action.clone() else {
            panic!("worktree add is synchronous");
        };
        let args = Args::new(tokens[used..].to_vec(), sandbox.clone());
        let result = with_window(cx, handle, |window, cx| handler(window, cx, &args));
        assert!(result.is_err(), "{path} is refused: {result:?}");
    }
    assert!(!outside.exists());
    assert_eq!(worktree_paths(&fx).len(), 3, "nothing was added");
}
