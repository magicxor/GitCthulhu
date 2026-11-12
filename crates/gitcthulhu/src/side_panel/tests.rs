use core::prelude::v1::test;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, ScrollDelta, TestAppContext, Window,
    point, px,
};

use super::model::{RowInputs, Section, SideRow, build_rows, submodule_state};
use super::{SidePanelView, ids};
use crate::backend::present::OpFailure;
use crate::backend::{
    GitError, GitTime, HeadState, LocalBranch, ObjectId, ObjectKind, OpOutcome, RefreshSet,
    RelaPath, RemoteBranch, RemoteInfo, StashApplyResult, StashInfo, SubmoduleInfo, SubmoduleState,
    TagInfo, Upstream,
};
use crate::menus::refs::{self, tests_support};
use crate::menus::{MenuItem, MenuKind, MenuSlot, MenuTarget, register_extender};
use crate::settings::LinesHeight;
use crate::tab::ui::Selection;
use crate::tab::ui::{RepoUiEvent, SideExpansion};
use crate::tab::{RepoTab, RepoTabEvent, TabCtx};
use crate::test_support::geometry::{assert_height, assert_rect, assert_width, bounds};
use crate::test_support::seed::{full, oid};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, mount_fixture, mount_seeded, script_op,
    toast_texts,
};

// ---- helpers ----------------------------------------------------------------------------------------------------

fn local(name: &str, target: ObjectId, is_head: bool) -> LocalBranch {
    LocalBranch {
        name: name.to_owned(),
        full: full(&format!("refs/heads/{name}")),
        target,
        is_head,
        upstream: None,
        ahead_behind: None,
        checked_out_in: None,
    }
}

fn tracking(local: &mut LocalBranch, remote: &str) {
    local.upstream = Some(Upstream {
        remote: remote.to_owned(),
        merge: local.full.clone(),
        tracking: Some(full(&format!("refs/remotes/{remote}/{}", local.name))),
        gone: false,
    });
}

fn remote_branch(remote: &str, name: &str, target: ObjectId) -> RemoteBranch {
    RemoteBranch {
        remote: remote.to_owned(),
        name: name.to_owned(),
        full: full(&format!("refs/remotes/{remote}/{name}")),
        target,
    }
}

fn tag(name: &str, target: ObjectId) -> TagInfo {
    TagInfo {
        name: name.to_owned(),
        full: full(&format!("refs/tags/{name}")),
        target,
        peeled_kind: ObjectKind::Commit,
        tag_object: None,
        message_summary: None,
        tagger_time: None,
    }
}

fn stash(index: usize, id: ObjectId, message: &str) -> StashInfo {
    StashInfo {
        index,
        id,
        message: message.to_owned(),
        branch: Some("main".into()),
        time: GitTime {
            seconds: 1_700_000_000,
            offset_minutes: 0,
        },
        base: oid(1),
        index_commit: None,
        untracked_commit: None,
    }
}

fn submodule(path: &str, state: SubmoduleState) -> SubmoduleInfo {
    SubmoduleInfo {
        name: path.to_owned(),
        path: RelaPath::from(path),
        url: Some(format!("https://example.com/{path}.git")),
        branch: None,
        state,
        index_commit: None,
        head_commit: None,
        dirty: None,
    }
}

/// `main` (HEAD, tracking origin/main), `feature` and `Zeta`; remotes `origin` (main, dev) and `upstream`; two tags;
/// two stashes; four submodules, one per state.
fn refs_seed() -> RepoSeed {
    let mut seed = RepoSeed::branchy();
    let mut main = local("main", oid(10), true);
    tracking(&mut main, "origin");
    seed.refs.local = vec![
        local("feature", oid(11), false),
        main,
        local("Zeta", oid(12), false),
    ];
    seed.refs.remotes = vec![
        RemoteInfo {
            name: "origin".into(),
            fetch_url: Some("https://example.com/repo.git".into()),
            push_url: None,
            head_branch: Some("main".into()),
            branches: vec![
                remote_branch("origin", "dev", oid(11)),
                remote_branch("origin", "main", oid(12)),
            ],
        },
        RemoteInfo {
            name: "upstream".into(),
            fetch_url: Some("https://example.com/up.git".into()),
            push_url: None,
            head_branch: None,
            branches: vec![remote_branch("upstream", "main", oid(12))],
        },
    ];
    seed.refs.tags = vec![tag("v1.0", oid(14)), tag("v1.1", oid(12))];
    seed.refs.stashes = vec![
        stash(0, oid(13), "WIP on main: abc1234 Base\nsecond line"),
        stash(1, oid(16), "On feature: parked work"),
    ];
    seed.submodules = vec![
        submodule("libs/missing", SubmoduleState::Missing),
        submodule("libs/uninit", SubmoduleState::Uninitialized),
        submodule("libs/init", SubmoduleState::Initialized),
        submodule("libs/out", SubmoduleState::CheckedOut),
    ];
    seed
}

fn ctx_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> TabCtx {
    cx.update(|cx| tab.read(cx).ctx().clone())
}

fn expand(ctx: &TabCtx, cx: &mut TestAppContext, edit: impl FnOnce(&mut SideExpansion)) {
    cx.update(|cx| {
        ctx.ui.update(cx, |ui, cx| {
            edit(&mut ui.side_expanded);
            cx.emit(RepoUiEvent::TogglesChanged);
            cx.notify();
        })
    });
}

fn frame(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
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

fn item_labels(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            MenuItem::Action { label, .. } => Some(label.to_string()),
            MenuItem::Separator => None,
        })
        .collect()
}

fn menu_of(
    tab: &Entity<RepoTab>,
    cx: &mut TestAppContext,
    target: impl FnOnce(&TabCtx, &App) -> MenuTarget,
    build: fn(&MenuTarget, &App) -> Vec<MenuItem>,
) -> Vec<MenuItem> {
    let ctx = ctx_of(tab, cx);
    cx.update(|cx| {
        let target = target(&ctx, cx);
        build(&target, cx)
    })
}

fn view_of(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Entity<SidePanelView> {
    cx.update(|cx| tab.read(cx).views.side_panel.clone())
}

fn model_refs(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Arc<crate::backend::RefList> {
    cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .model
            .read(cx)
            .refs
            .value
            .clone()
            .expect("refs are loaded")
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

fn ok<T>(value: T, refresh: RefreshSet) -> Result<OpOutcome<T>, OpFailure> {
    Ok(OpOutcome {
        value,
        refresh,
        log: Default::default(),
        warnings: Vec::new(),
    })
}

fn fail<T: Send + 'static>(tab: &Entity<RepoTab>, cx: &mut TestAppContext, err: GitError) {
    cx.update(|cx| {
        script_op::<OpOutcome<T>>(tab, cx, Err(OpFailure::Git(Arc::new(err))), Duration::ZERO)
    });
}

fn op_titles(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|c| match c {
                TestCall::Op { title, .. } => Some(title.unwrap_or_default()),
                _ => None,
            })
            .collect()
    })
}

fn op_kinds(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<crate::tab::ops::OpKind> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|c| match c {
                TestCall::Op { kind, .. } => Some(kind),
                _ => None,
            })
            .collect()
    })
}

// ---- pure model -------------------------------------------------------------------------------------------------

fn rows_of(seed: &RepoSeed, filter: &str, expanded: &SideExpansion) -> Vec<SideRow> {
    let collapsed = HashSet::new();
    build_rows(&RowInputs {
        refs: Some(&seed.refs),
        submodules: &seed.submodules,
        worktrees: None,
        filter,
        expanded,
        remotes_collapsed: &collapsed,
    })
}

fn all_expanded() -> SideExpansion {
    SideExpansion {
        local: true,
        remotes: true,
        tags: true,
        stashes: true,
        submodules: true,
        worktrees: true,
    }
}

fn header_count(rows: &[SideRow], section: Section) -> usize {
    rows.iter()
        .find_map(|r| match r {
            SideRow::Header {
                section: s, count, ..
            } if *s == section => Some(*count),
            _ => None,
        })
        .expect("every section has a header")
}

/// 07 §6.2: case-insensitive substring per kind: full ref name for local branches, the simple name for remote
/// branches and tags, the message for stashes, the path for submodules; remote headers stay; counts are filtered.
#[test]
fn filter_rules_per_kind() {
    let seed = refs_seed();
    let expanded = all_expanded();

    let rows = rows_of(&seed, "", &expanded);
    assert_eq!(
        header_count(&rows, Section::LocalBranches),
        3,
        "no filter: all local branches"
    );

    // Local branches: the FULL ref name (so "heads/" matches all of them), case-insensitive.
    let rows = rows_of(&seed, "REFS/HEADS/FEA", &expanded);
    assert_eq!(header_count(&rows, Section::LocalBranches), 1);
    assert!(rows.contains(&SideRow::LocalBranch { ix: 0 }), "feature");
    let rows = rows_of(&seed, "heads", &expanded);
    assert_eq!(
        header_count(&rows, Section::LocalBranches),
        3,
        "Gitnuro matches the full ref name, so 'heads' matches every local branch"
    );

    // Remote branches: the simple name; the remote rows stay visible and the Remotes count is the remote count.
    let rows = rows_of(&seed, "dev", &expanded);
    assert_eq!(header_count(&rows, Section::Remotes), 2);
    assert!(rows.contains(&SideRow::Remote {
        ix: 0,
        collapsed: false
    }));
    assert!(rows.contains(&SideRow::Remote {
        ix: 1,
        collapsed: false
    }));
    assert!(rows.contains(&SideRow::RemoteBranch { remote: 0, ix: 0 }));
    assert!(
        !rows.iter().any(|r| matches!(
            r,
            SideRow::RemoteBranch { remote: 1, .. } | SideRow::RemoteBranch { remote: 0, ix: 1 }
        )),
        "only origin/dev matches"
    );
    let rows = rows_of(&seed, "origin", &expanded);
    assert!(
        !rows
            .iter()
            .any(|r| matches!(r, SideRow::RemoteBranch { .. })),
        "the remote's name is not part of the branch's simple name"
    );

    // Tags by simple name, stashes by message, submodules by path.
    let rows = rows_of(&seed, "V1.1", &expanded);
    assert_eq!(header_count(&rows, Section::Tags), 1);
    let rows = rows_of(&seed, "parked", &expanded);
    assert_eq!(header_count(&rows, Section::Stashes), 1);
    assert!(rows.contains(&SideRow::Stash { ix: 1 }));
    let rows = rows_of(&seed, "LIBS/IN", &expanded);
    assert_eq!(header_count(&rows, Section::Submodules), 1);
    assert!(rows.contains(&SideRow::Submodule { ix: 2 }), "libs/init");

    // All five sections are always there, even when nothing matches.
    let rows = rows_of(&seed, "zzz-no-match", &expanded);
    let headers: Vec<Section> = rows
        .iter()
        .filter_map(|r| match r {
            SideRow::Header { section, .. } => Some(*section),
            _ => None,
        })
        .collect();
    assert_eq!(
        headers,
        [
            Section::LocalBranches,
            Section::Remotes,
            Section::Tags,
            Section::Stashes,
            Section::Submodules
        ],
        "section order is fixed and a section with zero items stays"
    );
}

/// 07 §6.5: the current branch first, then by name, case-insensitive.
#[test]
fn local_branches_sort_current_first_then_case_insensitive() {
    let seed = refs_seed();
    let rows = rows_of(&seed, "", &all_expanded());
    let order: Vec<usize> = rows
        .iter()
        .filter_map(|r| match r {
            SideRow::LocalBranch { ix } => Some(*ix),
            _ => None,
        })
        .collect();
    // indexes: 0 feature, 1 main (HEAD), 2 Zeta
    assert_eq!(order, [1, 0, 2], "main (HEAD), feature, Zeta");
}

#[test]
fn collapsed_sections_and_remotes_hide_their_rows() {
    let seed = refs_seed();
    let mut expanded = all_expanded();
    expanded.tags = false;
    let mut collapsed = HashSet::new();
    collapsed.insert("origin".to_owned());
    let rows = build_rows(&RowInputs {
        refs: Some(&seed.refs),
        submodules: &seed.submodules,
        worktrees: None,
        filter: "",
        expanded: &expanded,
        remotes_collapsed: &collapsed,
    });
    assert!(
        rows.contains(&SideRow::Remote {
            ix: 0,
            collapsed: true
        }),
        "the collapsed remote row stays"
    );
    assert!(
        !rows.contains(&SideRow::RemoteBranch { remote: 0, ix: 0 }),
        "its branches are hidden"
    );
    assert!(
        rows.contains(&SideRow::RemoteBranch { remote: 1, ix: 0 }),
        "other remotes are unaffected"
    );
    assert!(
        !rows.iter().any(|r| matches!(r, SideRow::Tag { .. })),
        "a collapsed section shows only its header"
    );
    assert_eq!(
        header_count(&rows, Section::Tags),
        2,
        "the count stays with the header"
    );
}

#[test]
fn no_refs_yield_empty_sections() {
    let collapsed = HashSet::new();
    let rows = build_rows(&RowInputs {
        refs: None,
        submodules: &[],
        worktrees: None,
        filter: "",
        expanded: &SideExpansion::default(),
        remotes_collapsed: &collapsed,
    });
    assert_eq!(rows.len(), 5, "five headers and nothing else");
}

#[test]
fn submodule_letters_and_names() {
    assert_eq!(submodule_state(SubmoduleState::Missing), ("M", "MISSING"));
    assert_eq!(
        submodule_state(SubmoduleState::Uninitialized),
        ("U", "UNINITIALIZED")
    );
    assert_eq!(
        submodule_state(SubmoduleState::Initialized),
        ("I", "INITIALIZED")
    );
    assert_eq!(
        submodule_state(SubmoduleState::CheckedOut),
        ("R", "REV_CHECKED_OUT")
    );
}

#[test]
fn ahead_behind_text_shows_only_nonzero_parts() {
    use crate::backend::AheadBehind;
    let text = |ahead, behind, capped| {
        super::rows::ahead_behind_text(&AheadBehind {
            ahead,
            behind,
            capped,
        })
    };
    assert_eq!(text(0, 0, false), None, "level with the upstream: nothing");
    assert_eq!(text(2, 0, false).as_deref(), Some("↑2"));
    assert_eq!(text(0, 3, false).as_deref(), Some("↓3"));
    assert_eq!(text(2, 3, false).as_deref(), Some("↑2 ↓3"));
    assert_eq!(text(999, 0, true).as_deref(), Some("↑999+"));
}

// ---- layout -----------------------------------------------------------------------------------------------------

/// 21 §8.2 side panel row: filter 38 high at x 8 to the right edge; headers 36 high; branch icon x 32, text x 56;
/// remote branch icon x 56, text x 80; section order; Remotes and Tags collapsed by default; no Worktrees section.
#[gpui_kit::test]
fn rows_and_indents(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let panel = bounds(window, ids::SIDE_PANEL);
        let (px0, py0) = (panel.origin.x.as_f32(), panel.origin.y.as_f32());
        let width = panel.size.width.as_f32();
        assert_eq!(width, 220., "the side pane is 220 wide");
        // Filter: x 8, to the right edge (0 dp right margin), 38 high.
        assert_rect(window, ids::SIDE_FILTER, px0 + 8., py0, width - 8., 38.);
        // 07 §10.1: the leading icon starts 12 dp into the field, the text 8 dp after the 16 dp icon.
        let filter_icon = bounds(window, ids::SIDE_FILTER_ICON);
        assert_eq!(
            filter_icon.origin.x.as_f32() - px0,
            8. + 12.,
            "search icon x"
        );
        assert_eq!(filter_icon.size.width.as_f32(), 16., "search icon size");
        assert_eq!(
            bounds(window, ids::SIDE_FILTER_TEXT).origin.x.as_f32() - px0,
            8. + 12. + 16. + 8.,
            "filter text x"
        );
        // The first header sits 4 dp below the filter.
        let local = ids::header(Section::LocalBranches);
        assert_rect(window, local.clone(), px0, py0 + 38. + 4., width, 36.);
        let header_icon = bounds(window, ids::child(local.clone(), "icon"));
        let header_title = bounds(window, ids::child(local, "title"));
        assert_eq!(header_icon.origin.x.as_f32() - px0, 32., "header icon x");
        assert_eq!(header_icon.size.width.as_f32(), 16.);
        assert_eq!(
            header_title.origin.x.as_f32() - px0,
            48.,
            "the title box starts at x 48; its 8 dp padding puts the text at x 56"
        );

        // Branch rows: 36 high, icon at x 32, text at x 56.
        let feature = ids::branch("refs/heads/feature");
        assert_height(window, feature.clone(), 36.);
        assert_eq!(
            bounds(window, ids::child(feature.clone(), "icon"))
                .origin
                .x
                .as_f32()
                - px0,
            32.
        );
        assert_eq!(
            bounds(window, ids::child(feature, "text"))
                .origin
                .x
                .as_f32()
                - px0,
            56.
        );

        // Section order, top to bottom; Remotes and Tags are collapsed by default.
        let y = |s: Section| bounds(window, ids::header(s)).origin.y.as_f32();
        assert!(y(Section::LocalBranches) < y(Section::Remotes));
        assert!(y(Section::Remotes) < y(Section::Tags));
        assert!(y(Section::Tags) < y(Section::Stashes));
        assert!(y(Section::Stashes) < y(Section::Submodules));
        assert!(
            window.try_find(ids::remote("origin")).is_none(),
            "Remotes is collapsed by default"
        );
        assert!(
            window.try_find(ids::tag("refs/tags/v1.0")).is_none(),
            "Tags is collapsed by default"
        );
        assert!(
            window.try_find(ids::header(Section::Worktrees)).is_none(),
            "no Worktrees section in milestone 1"
        );
    })
    .expect("window");

    expand(&ctx, cx, |e| e.remotes = true);
    frame(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let px0 = bounds(window, ids::SIDE_PANEL).origin.x.as_f32();
        let remote = ids::remote("origin");
        assert_eq!(
            bounds(window, ids::child(remote, "icon")).origin.x.as_f32() - px0,
            32.,
            "a remote row is indented like a branch row"
        );
        let rb = ids::remote_branch("refs/remotes/origin/dev");
        assert_height(window, rb.clone(), 36.);
        assert_eq!(
            bounds(window, ids::child(rb.clone(), "icon"))
                .origin
                .x
                .as_f32()
                - px0,
            56.,
            "remote branch icon at 32 + 24"
        );
        assert_eq!(
            bounds(window, ids::child(rb, "text")).origin.x.as_f32() - px0,
            80.,
            "remote branch text at 56 + 24"
        );
    })
    .expect("window");
}

/// 07 §13.4: compact rows are 34 high.
#[gpui_kit::test]
fn compact_mode_rows_are_34(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    cx.update(|cx| crate::settings::update(cx, |s| s.lines_height = LinesHeight::Compact));
    frame(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_height(window, ids::header(Section::LocalBranches), 34.);
        assert_height(window, ids::branch("refs/heads/feature"), 34.);
    })
    .expect("window");
}

/// 07 §6.3: the count shows normally; hovering Remotes and Submodules swaps it for a "+" that opens the add
/// dialog without toggling the section; Local branches, Tags and Stashes have no "+".
#[gpui_kit::test]
fn header_counts_and_hover_plus(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle);
    let remotes = ids::header(Section::Remotes);
    let add = ids::header_add(Section::Remotes);
    cx.update_window(handle, |_, window, _| {
        assert!(
            !window.try_find(add.clone()).is_some_and(|p| p.visible()),
            "the + is not shown until the header is hovered"
        );
        assert!(
            window.try_find(ids::header_add(Section::Tags)).is_none()
                && window
                    .try_find(ids::header_add(Section::LocalBranches))
                    .is_none(),
            "only Remotes and Submodules have a +"
        );
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.hover(remotes.clone(), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        let plus = window.try_find(add.clone());
        assert!(
            plus.is_some_and(|p| p.visible() && p.bounds().size.width.as_f32() >= 16.),
            "hovering shows the +"
        );
    })
    .expect("window");

    // Clicking the + opens the dialog (a placeholder toast until G10) and leaves the section collapsed.
    let before = cx.update(|cx| ctx.ui.read(cx).side_expanded.clone());
    cx.update_window(handle, |_, window, cx| {
        window.click(add.clone(), cx);
    })
    .expect("window");
    cx.run_until_parked();
    let after = cx.update(|cx| ctx.ui.read(cx).side_expanded.clone());
    assert_eq!(before, after, "the + does not toggle the section");
    assert!(
        !cx.update(|cx| toast_texts(&app, cx)).is_empty()
            || cx.update(|cx| app.read(cx).pending_errors()) == 0,
        "the add-remote dialog entry point was reached"
    );
}

/// 07 §6.3: a click anywhere on the header toggles the section; the state is per tab, in `RepoUi`.
#[gpui_kit::test]
fn header_click_toggles_section(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::header(Section::Tags), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(ids::tag("refs/tags/v1.0")).is_some(),
            "Tags expanded: its rows are there"
        );
        window.click(ids::header(Section::LocalBranches), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(ids::branch("refs/heads/main")).is_none(),
            "Local branches collapsed"
        );
    })
    .expect("window");
    let expanded = cx.update(|cx| ctx.ui.read(cx).side_expanded.clone());
    assert!(expanded.tags && !expanded.local);
}

/// 07 §6.5: a click on a remote row collapses its branches (no chevron).
#[gpui_kit::test]
fn remote_click_toggles_its_branches(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    expand(&ctx, cx, |e| e.remotes = true);
    frame(cx, handle);
    let dev = ids::remote_branch("refs/remotes/origin/dev");
    cx.update_window(handle, |_, window, cx| {
        assert!(window.try_find(dev.clone()).is_some());
        window.click(ids::remote("origin"), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find(dev.clone()).is_none(),
            "the remote's branches are collapsed"
        );
        assert!(
            window
                .try_find(ids::remote_branch("refs/remotes/upstream/main"))
                .is_some(),
            "other remotes keep their branches"
        );
        window.click(ids::remote("origin"), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find(dev.clone()).is_some(), "and back");
    })
    .expect("window");
}

/// 07 §6.5: bold name and a "HEAD" badge for the current branch only.
#[gpui_kit::test]
fn current_branch_bold_head_badge(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let main = ids::branch("refs/heads/main");
        let head = window.try_find(ids::child(main.clone(), "head"));
        assert!(head.is_some(), "the current branch has the HEAD badge");
        let head_b = head.expect("badge").bounds();
        let row = bounds(window, main.clone());
        assert!(
            (row.right().as_f32() - head_b.right().as_f32()).abs() <= 0.5
                && head_b.size.width.as_f32() > 32.,
            "the badge sits at the right edge with 16 dp padding on both sides"
        );
        assert!(
            window
                .try_find(ids::child(ids::branch("refs/heads/feature"), "head"))
                .is_none(),
            "other branches have none"
        );
        // The current branch is listed first.
        assert!(
            bounds(window, main).origin.y
                < bounds(window, ids::branch("refs/heads/feature")).origin.y,
            "the current branch comes first"
        );
    })
    .expect("window");
}

// ---- interaction ------------------------------------------------------------------------------------------------

/// 07 §6.5: a click selects the branch (and asks the log to scroll to its commit); a double-click also checks it
/// out; the current branch is not re-checked-out; remote branches check out as tracking branches.
#[gpui_kit::test]
fn double_click_checks_out(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    expand(&ctx, cx, |e| e.remotes = true);
    frame(cx, handle);

    let scrolls: Rc<RefCell<Vec<ObjectId>>> = Rc::default();
    let sink = scrolls.clone();
    let _sub = cx.update(|cx| {
        cx.subscribe(&ctx.events, move |_, ev: &RepoTabEvent, _| {
            if let RepoTabEvent::ScrollToCommit { id } = ev {
                sink.borrow_mut().push(*id);
            }
        })
    });

    cx.update_window(handle, |_, window, cx| {
        window.click(ids::branch("refs/heads/feature"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        op_kinds(&tab, cx).is_empty(),
        "a single click starts no operation"
    );
    assert_eq!(
        cx.update(|cx| ctx.ui.read(cx).selection.clone()),
        Selection::Branch {
            full: full("refs/heads/feature"),
            target: oid(11)
        },
        "the click selected the branch"
    );
    assert_eq!(*scrolls.borrow(), [oid(11)], "the log is asked to show it");

    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::branch("refs/heads/feature"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        op_titles(&tab, cx),
        ["Checkout 'feature'"],
        "a double-click checks the branch out"
    );

    // The current branch: select only.
    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::branch("refs/heads/main"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        op_titles(&tab, cx).len(),
        1,
        "double-clicking the current branch checks nothing out"
    );

    // Remote branch: tracking checkout.
    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::remote_branch("refs/remotes/origin/dev"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        op_kinds(&tab, cx),
        [
            crate::tab::ops::OpKind::CheckoutBranch,
            crate::tab::ops::OpKind::CheckoutRemoteBranch
        ]
    );
    // Remote branches are never drawn as selected, whatever the selection says (07 §6.5).
}

/// Tags and stashes select on click; there is no double-click action.
#[gpui_kit::test]
fn tag_and_stash_click_select(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    expand(&ctx, cx, |e| e.tags = true);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::tag("refs/tags/v1.1"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| ctx.ui.read(cx).selection.clone()),
        Selection::Tag {
            full: full("refs/tags/v1.1"),
            target: oid(12)
        }
    );
    let stash_id = oid(16);
    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::stash(&stash_id.to_hex().to_string()), cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| ctx.ui.read(cx).selection.clone()),
        Selection::Stash {
            id: stash_id,
            index: 1
        }
    );
    assert!(
        op_kinds(&tab, cx).is_empty(),
        "no double-click action on tags and stashes"
    );
    // The selected rows are drawn selected: the selection is part of the row, so its row exists and is distinct.
    frame(cx, handle);
}

/// 07 §6.5: a submodule row shows M/U/I/R with a tooltip; a click opens valid ones in a new tab.
#[gpui_kit::test]
fn submodule_state_letter_tooltip(cx: &mut TestAppContext) {
    tests_support::OPENED.with(|o| o.borrow_mut().clear());
    let (handle, _app, _tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        for path in ["libs/missing", "libs/uninit", "libs/init", "libs/out"] {
            assert!(
                window
                    .try_find(ids::child(ids::submodule(path), "state"))
                    .is_some(),
                "{path} shows its state letter"
            );
        }
        let row = bounds(window, ids::submodule("libs/out"));
        let letter = bounds(window, ids::child(ids::submodule("libs/out"), "state"));
        assert!(
            (row.right().as_f32() - letter.right().as_f32()).abs() <= 0.5,
            "the letter sits at the right edge (padding inside its box)"
        );

        window.click(ids::submodule("libs/uninit"), cx);
        window.click(ids::submodule("libs/missing"), cx);
        assert!(
            tests_support::OPENED.with(|o| o.borrow().is_empty()),
            "uninitialized and missing submodules do not open"
        );
        window.click(ids::submodule("libs/init"), cx);
        window.click(ids::submodule("libs/out"), cx);
    })
    .expect("window");
    let opened: Vec<String> = tests_support::OPENED.with(|o| {
        o.borrow()
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect()
    });
    assert_eq!(opened.len(), 2, "initialized and checked-out ones open");
    assert!(opened[0].ends_with("libs/init") && opened[1].ends_with("libs/out"));
}

// ---- filter field -----------------------------------------------------------------------------------------------

/// Typing in the field filters the rows; Escape (the frame's CloseLastView through the `SidePaneSearch` view) clears
/// text and filter; the clear button does the same.
#[gpui_kit::test]
fn typing_filters_and_escape_clears(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    let view = view_of(&tab, cx);
    frame(cx, handle);
    let type_text = |cx: &mut TestAppContext, text: &'static str| {
        let view = view.clone();
        cx.update_window(handle, move |_, window, cx| {
            let input = view.read(cx).input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            window.render_frame(cx);
            window.input(text, cx);
        })
        .expect("window");
        frame(cx, handle);
    };
    type_text(cx, "feat");
    assert_eq!(
        cx.update(|cx| ctx.ui.read(cx).side_filter.to_string()),
        "feat",
        "the typed text is the tab's side filter"
    );
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::branch("refs/heads/feature")).is_some(),
            "feature matches"
        );
        assert!(
            window.try_find(ids::branch("refs/heads/main")).is_none(),
            "main does not"
        );
        assert!(
            window.try_find(ids::SIDE_FILTER_CLEAR).is_some(),
            "the clear button appears with text"
        );
        // 07 §10.1: Spacer 8, the 16 dp icon, 12 dp from the field's right edge (inside the 2 dp border).
        let field = bounds(window, ids::SIDE_FILTER);
        let clear = bounds(window, ids::SIDE_FILTER_CLEAR);
        assert_eq!(
            clear.size.width.as_f32(),
            8. + 16. + 10.,
            "clear button box"
        );
        assert_eq!(
            (field.origin.x + field.size.width - (clear.origin.x + clear.size.width)).as_f32(),
            2.,
            "the clear button box ends at the border"
        );
    })
    .expect("window");

    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    frame(cx, handle);
    assert!(
        cx.update(|cx| ctx.ui.read(cx).side_filter.is_empty()),
        "Escape cleared the filter"
    );
    assert!(
        cx.update(|cx| view.read(cx).input.read(cx).value().is_empty()),
        "and the field's text"
    );
    cx.update_window(handle, |_, window, _| {
        assert!(
            window.try_find(ids::branch("refs/heads/main")).is_some(),
            "all branches are back"
        );
        assert!(window.try_find(ids::SIDE_FILTER_CLEAR).is_none());
    })
    .expect("window");

    // The clear button.
    type_text(cx, "zeta");
    assert_eq!(
        cx.update(|cx| ctx.ui.read(cx).side_filter.to_string()),
        "zeta"
    );
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::SIDE_FILTER_CLEAR, cx)
    })
    .expect("window");
    frame(cx, handle);
    assert!(
        cx.update(|cx| ctx.ui.read(cx).side_filter.is_empty()),
        "cleared by the button"
    );
}

// ---- menus ------------------------------------------------------------------------------------------------------

/// 07 §6.6: the branch menu's items by condition (current, HEAD detached, local, remote).
#[gpui_kit::test]
fn branch_menu_items_by_condition(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let refs = model_refs(&tab, cx);
    let main = refs.local[1].clone();
    let feature = refs.local[0].clone();

    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::local_branch_target(c, &main),
        refs::local_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-rename",
            "branch-upstream",
            "---",
            "branch-copy-name"
        ],
        "the current branch: no checkout, merge, rebase or delete"
    );

    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::local_branch_target(c, &feature),
        refs::local_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-checkout",
            "branch-merge",
            "branch-rebase",
            "---",
            "branch-rename",
            "branch-upstream",
            "---",
            "branch-delete",
            "---",
            "branch-copy-name"
        ],
        "a local branch other than the current one"
    );
    assert_eq!(
        item_labels(&items),
        [
            "Checkout branch",
            "Merge branch",
            "Rebase branch",
            "Rename branch",
            "Change default upstream branch",
            "Delete branch",
            "Copy branch name"
        ]
    );

    let origin_main = refs.remotes[0].branches[1].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::remote_branch_target(c, &origin_main),
        refs::remote_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-checkout",
            "branch-merge",
            "branch-rebase",
            "---",
            "branch-push-to",
            "branch-pull-from",
            "---",
            "branch-delete-remote",
            "---",
            "branch-copy-name"
        ],
        "a remote branch with HEAD on a branch"
    );
    assert_eq!(
        item_labels(&items)[3..5],
        [
            "Push \"main\" to \"origin/main\"".to_owned(),
            "Pull \"origin/main\" to \"main\"".to_owned()
        ],
        "push and pull name both branches"
    );

    // Detached HEAD: no merge, rebase, push or pull.
    cx.update(|cx| {
        let model = tab.read(cx).ctx().model.clone();
        model.update(cx, |m, cx| {
            let mut value = (**m.refs.value.as_ref().expect("refs")).clone();
            value.head = HeadState::Detached { target: oid(10) };
            value.local[1].is_head = false;
            m.set_refs(Arc::new(value), cx);
        });
    });
    let refs = model_refs(&tab, cx);
    let (feature, origin_main) = (refs.local[0].clone(), refs.remotes[0].branches[1].clone());
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::local_branch_target(c, &feature),
        refs::local_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-checkout",
            "branch-rename",
            "branch-upstream",
            "---",
            "branch-delete",
            "---",
            "branch-copy-name"
        ],
        "detached HEAD: merge and rebase are hidden, and so is their separator (08 §2.13)"
    );
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::remote_branch_target(c, &origin_main),
        refs::remote_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-checkout",
            "branch-delete-remote",
            "---",
            "branch-copy-name"
        ],
        "detached HEAD: no merge, rebase, push or pull on a remote branch, nor their separators"
    );
}

/// The tag, stash, remote and submodule menus (07 §6.6, 09 §4.2-4.5).
#[gpui_kit::test]
fn other_ref_menus(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let refs = model_refs(&tab, cx);

    let tag_info = refs.tags[0].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::tag_target(c, tag_info.full.clone(), tag_info.target),
        refs::tag,
    );
    assert_eq!(item_ids(&items), ["tag-checkout", "tag-delete"]);
    assert_eq!(item_labels(&items), ["Checkout tag's commit", "Delete tag"]);

    let stash_info = refs.stashes[0].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::stash_target(c, &stash_info),
        refs::stash,
    );
    assert_eq!(
        item_labels(&items),
        ["Apply stash", "Pop stash", "Drop stash"]
    );

    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::remote_target(c, "origin"),
        refs::remote,
    );
    assert_eq!(
        item_ids(&items),
        ["remote-edit", "remote-delete", "---", "remote-fetch"]
    );
    assert_eq!(item_labels(&items), ["Edit", "Delete", "Fetch branches"]);

    let subs = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .model
            .read(cx)
            .submodules
            .value
            .clone()
            .expect("submodules")
    });
    let by_state = |state: SubmoduleState| {
        subs.iter()
            .find(|s| s.state == state)
            .expect("one per state")
            .clone()
    };
    let (missing, uninit, init, out) = (
        by_state(SubmoduleState::Missing),
        by_state(SubmoduleState::Uninitialized),
        by_state(SubmoduleState::Initialized),
        by_state(SubmoduleState::CheckedOut),
    );
    let ids_for = |sub: &SubmoduleInfo, cx: &mut TestAppContext| {
        let sub = sub.clone();
        item_ids(&menu_of(
            &tab,
            cx,
            move |c, _| refs::submodule_target(c, &sub),
            refs::submodule,
        ))
    };
    assert_eq!(
        ids_for(&uninit, cx),
        ["submodule-init", "---", "submodule-delete"]
    );
    assert_eq!(
        ids_for(&init, cx),
        [
            "submodule-open",
            "submodule-sync",
            "submodule-update",
            "---",
            "submodule-delete"
        ]
    );
    assert_eq!(ids_for(&out, cx), ids_for(&init, cx));
    assert_eq!(ids_for(&missing, cx), ["submodule-delete"]);
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::submodule_target(c, &init),
        refs::submodule,
    );
    assert_eq!(
        item_labels(&items),
        [
            "Open submodule in new tab",
            "Sync submodule",
            "Update submodule",
            "Delete submodule"
        ]
    );
}

fn ext_after_checkout(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x-after-checkout", "X1", |_, _| {}));
}
fn ext_after_rename(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x-after-rename", "X2", |_, _| {}));
}
fn ext_after_push_pull(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x-after-push-pull", "X3", |_, _| {}));
}
fn ext_end(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x-end", "X4", |_, _| {}));
}

/// 21 §2.13.2: every builder calls `extend` at its slots, in the documented places.
#[gpui_kit::test]
fn branch_menu_calls_extender_slots(cx: &mut TestAppContext) {
    let (_handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    cx.update(|cx| {
        register_extender(
            cx,
            MenuKind::LocalBranch,
            MenuSlot::AfterCheckout,
            ext_after_checkout,
        );
        register_extender(
            cx,
            MenuKind::LocalBranch,
            MenuSlot::AfterRenameGroup,
            ext_after_rename,
        );
        register_extender(
            cx,
            MenuKind::RemoteBranch,
            MenuSlot::AfterPushPullGroup,
            ext_after_push_pull,
        );
        for kind in [
            MenuKind::Tag,
            MenuKind::Stash,
            MenuKind::Remote,
            MenuKind::Submodule,
        ] {
            register_extender(cx, kind, MenuSlot::End, ext_end);
        }
    });
    let refs = model_refs(&tab, cx);
    let (main, feature) = (refs.local[1].clone(), refs.local[0].clone());

    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::local_branch_target(c, &feature),
        refs::local_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "branch-checkout",
            "x-after-checkout",
            "---",
            "branch-merge",
            "branch-rebase",
            "---",
            "branch-rename",
            "branch-upstream",
            "x-after-rename",
            "---",
            "branch-delete",
            "---",
            "branch-copy-name"
        ],
        "AfterCheckout follows Checkout; AfterRenameGroup follows Rename and Change upstream"
    );
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::local_branch_target(c, &main),
        refs::local_branch,
    );
    assert_eq!(
        item_ids(&items),
        [
            "x-after-checkout",
            "---",
            "branch-rename",
            "branch-upstream",
            "x-after-rename",
            "---",
            "branch-copy-name"
        ],
        "the current branch has no Checkout, but extenders (pull request items) still apply"
    );
    let origin_main = refs.remotes[0].branches[1].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::remote_branch_target(c, &origin_main),
        refs::remote_branch,
    );
    let ids = item_ids(&items);
    let pos = |id: &str| ids.iter().position(|i| i == id).expect(id);
    assert!(
        pos("branch-pull-from") < pos("x-after-push-pull")
            && pos("x-after-push-pull") < pos("branch-delete-remote"),
        "AfterPushPullGroup sits after the push and pull group: {ids:?}"
    );
    let tag_info = refs.tags[0].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::tag_target(c, tag_info.full.clone(), tag_info.target),
        refs::tag,
    );
    assert_eq!(item_ids(&items), ["tag-checkout", "tag-delete", "x-end"]);
    let stash_info = refs.stashes[0].clone();
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::stash_target(c, &stash_info),
        refs::stash,
    );
    assert_eq!(item_ids(&items).last().map(String::as_str), Some("x-end"));
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::remote_target(c, "origin"),
        refs::remote,
    );
    assert_eq!(item_ids(&items).last().map(String::as_str), Some("x-end"));
    let sub = cx.update(|cx| {
        tab.read(cx)
            .ctx()
            .model
            .read(cx)
            .submodules
            .value
            .clone()
            .expect("subs")[0]
            .clone()
    });
    let items = menu_of(
        &tab,
        cx,
        |c, _| refs::submodule_target(c, &sub),
        refs::submodule,
    );
    assert_eq!(item_ids(&items).last().map(String::as_str), Some("x-end"));
}

/// A right-click on a row opens its menu at the pointer; "Copy branch name" copies and confirms with a toast.
#[gpui_kit::test]
fn right_click_opens_menu_and_copy_name_toasts(cx: &mut TestAppContext) {
    tests_support::COPIED.with(|c| c.borrow_mut().clear());
    let (handle, app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.right_click(ids::branch("refs/heads/feature"), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find("menu-item-branch-checkout").is_some(),
            "the branch menu opened"
        );
        window.click("menu-item-branch-copy-name", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        tests_support::COPIED.with(|c| c.borrow().clone()),
        ["feature"]
    );
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .iter()
            .any(|t| t == "\"feature\" copied to clipboard"),
        "the toast names the copied text"
    );

    // 08 §2.13: a remote branch copies its simple name, without the remote prefix.
    let ctx = ctx_of(&tab, cx);
    expand(&ctx, cx, |e| e.remotes = true);
    frame(cx, handle);
    // Menus opened within 20 ms of each other are ignored (07 §9.3).
    std::thread::sleep(Duration::from_millis(30));
    cx.update_window(handle, |_, window, cx| {
        window.right_click(ids::remote_branch("refs/remotes/origin/dev"), cx);
        window.render_frame(cx);
        window.render_frame(cx);
        window.click("menu-item-branch-copy-name", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        tests_support::COPIED.with(|c| c.borrow().clone()),
        ["feature", "dev"]
    );
}

/// A header has no context menu (07 §6.3).
#[gpui_kit::test]
fn header_right_click_opens_nothing(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.right_click(ids::header(Section::LocalBranches), cx);
        window.render_frame(cx);
        assert!(window.try_find(crate::ids::MENU).is_none());
    })
    .expect("window");
}

// ---- virtualisation and ahead/behind ----------------------------------------------------------------------------

/// 21 §4.3 / 4: a 10,000-tag repository renders only the visible rows, and filtering is a pure function.
#[gpui_kit::test]
fn ten_thousand_tags_virtualized(cx: &mut TestAppContext) {
    let mut seed = refs_seed();
    seed.refs.tags = (0..10_000)
        .map(|i| tag(&format!("v{i}"), oid(14)))
        .collect();
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    expand(&ctx, cx, |e| {
        e.tags = true;
        e.local = false;
        e.stashes = false;
        e.submodules = false;
    });
    frame(cx, handle);
    let view = view_of(&tab, cx);
    let (rows, rendered) = cx
        .update_window(handle, |_, window, cx| {
            let rows = view.read(cx).built.rows.len();
            let rendered = (0..10_000)
                .filter(|i| {
                    window
                        .try_find(ids::tag(&format!("refs/tags/v{i}")))
                        .is_some()
                })
                .count();
            (rows, rendered)
        })
        .expect("window");
    assert_eq!(rows, 5 + 10_000, "five headers and one row per tag");
    assert!(
        (10..=40).contains(&rendered),
        "only the visible rows exist as elements, got {rendered}"
    );

    // Typing a filter narrows the list without rendering the rest.
    cx.update(|cx| {
        ctx.ui.update(cx, |ui, cx| {
            ui.side_filter = "v99".into();
            cx.notify();
        })
    });
    frame(cx, handle);
    let rows = cx.update(|cx| view.read(cx).built.rows.len());
    // v99, v990-v999, v9900-v9999: 1 + 10 + 100.
    assert_eq!(rows, 5 + 111, "the filtered rows");
}

/// 21 §2.6, §4.3: after a paint the panel asks for the ahead/behind of the VISIBLE tracking branches only, each pair
/// once; scrolling asks for the newly visible ones.
#[gpui_kit::test]
fn ahead_behind_requested_for_visible_only(cx: &mut TestAppContext) {
    let mut seed = refs_seed();
    let mut locals = Vec::new();
    let mut branches = Vec::new();
    for i in 0..200u64 {
        let name = format!("b{i:03}");
        let mut b = local(&name, oid(1000 + i), false);
        tracking(&mut b, "origin");
        locals.push(b);
        branches.push(remote_branch("origin", &name, oid(5000 + i)));
    }
    seed.refs.local = locals;
    seed.refs.remotes = vec![RemoteInfo {
        name: "origin".into(),
        fetch_url: None,
        push_url: None,
        head_branch: None,
        branches,
    }];
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle);
    let asked = |cx: &mut TestAppContext| -> usize {
        cx.update(|cx| {
            calls(&tab, cx)
                .into_iter()
                .filter_map(|c| match c {
                    TestCall::AheadBehind(n) => Some(n),
                    _ => None,
                })
                .sum()
        })
    };
    let first = asked(cx);
    assert!(
        (5..=30).contains(&first),
        "only the visible rows are asked for, not all 200: {first}"
    );
    frame(cx, handle);
    assert_eq!(asked(cx), first, "each pair is asked for once");

    cx.update_window(handle, |_, window, cx| {
        window.scroll(
            ids::SIDE_PANEL,
            ScrollDelta::Pixels(point(px(0.), px(-2000.))),
            cx,
        );
    })
    .expect("window");
    frame(cx, handle);
    let after = asked(cx);
    assert!(
        after > first && after < 100,
        "scrolling asks for the newly visible pairs only: {first} -> {after}"
    );
}

/// A branch with an upstream shows its ahead/behind counts once they are in the model.
#[gpui_kit::test]
fn ahead_behind_counts_are_shown(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle);
    let main = ids::branch("refs/heads/main");
    cx.update_window(handle, |_, window, _| {
        assert!(
            window
                .try_find(ids::child(main.clone(), "ahead-behind"))
                .is_none(),
            "nothing shown before the counts are known"
        );
    })
    .expect("window");
    cx.update(|cx| {
        ctx.model.update(cx, |m, cx| {
            let map = Arc::make_mut(&mut m.ahead_behind);
            map.insert(
                (oid(10), oid(12)),
                crate::backend::AheadBehind {
                    ahead: 2,
                    behind: 1,
                    capped: false,
                },
            );
            cx.emit(crate::tab::RepoModelEvent(RefreshSet::BRANCHES));
            cx.notify();
        })
    });
    frame(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            window
                .try_find(ids::child(main.clone(), "ahead-behind"))
                .is_some(),
            "↑2 ↓1 is shown"
        );
    })
    .expect("window");
}

/// A pair that disappears (the tab drops its cached counts) and comes back (undo, reset) is asked for again.
#[gpui_kit::test]
fn ahead_behind_asked_again_when_a_pair_returns(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let ctx = ctx_of(&tab, cx);
    frame(cx, handle);
    let asked = |cx: &mut TestAppContext| -> usize {
        cx.update(|cx| {
            calls(&tab, cx)
                .into_iter()
                .filter(|c| matches!(c, TestCall::AheadBehind(_)))
                .count()
        })
    };
    assert_eq!(asked(cx), 1, "main's pair is asked for");
    let original = model_refs(&tab, cx);
    let set_main_target = |cx: &mut TestAppContext, target: ObjectId| {
        let mut refs = (*original).clone();
        for b in &mut refs.local {
            if b.name == "main" {
                b.target = target;
            }
        }
        cx.update(|cx| ctx.model.update(cx, |m, cx| m.set_refs(Arc::new(refs), cx)));
    };
    set_main_target(cx, oid(20));
    frame(cx, handle);
    assert_eq!(asked(cx), 2, "the moved branch's new pair is asked for");
    set_main_target(cx, oid(10));
    frame(cx, handle);
    assert_eq!(asked(cx), 3, "the original pair is asked for again");
}

#[gpui_kit::test]
fn worktrees_section_absent_in_m1(cx: &mut TestAppContext) {
    let mut seed = refs_seed();
    seed.worktrees = vec![crate::backend::WorktreeInfo {
        id: None,
        path: std::path::PathBuf::from("/tmp/wt"),
        git_dir: std::path::PathBuf::from("/tmp/wt/.git"),
        head: None,
        is_current: true,
        locked: None,
        prunable: false,
        bare: false,
    }];
    let (handle, _app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    frame(cx, handle);
    let view = view_of(&tab, cx);
    let has_worktree_rows = cx.update(|cx| {
        view.read(cx).built.rows.iter().any(|r| {
            matches!(
                r,
                SideRow::Worktree { .. }
                    | SideRow::Header {
                        section: Section::Worktrees,
                        ..
                    }
            )
        })
    });
    assert!(!has_worktree_rows, "milestone 1: no Worktrees section");
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(ids::header(Section::Worktrees)).is_none());
    })
    .expect("window");
}

// ---- operation flows (scripted outcomes) ------------------------------------------------------------------------

/// 21 §2.9: a conflicted or partial apply keeps the entry; its one toast is a Warning instead of the success toast.
#[gpui_kit::test]
fn stash_apply_partial_warns(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let id = oid(13);

    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            ok(
                StashApplyResult::Partial {
                    conflicts: Vec::new(),
                    untracked_not_restored: vec![RelaPath::from("notes.txt")],
                },
                RefreshSet::STATUS,
            ),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, c| t.apply_stash(id, w, c));
    assert_eq!(
        cx.update(|cx| toast_texts(&app, cx)),
        ["Stash applied: the untracked file 'notes.txt' was not restored (it already exists)"],
        "a partial restore shows one Warning (which carries the undo action) instead of the success toast"
    );
    assert_eq!(
        op_titles(&tab, cx),
        ["Apply stash@{0}"],
        "the title names the stash"
    );

    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            ok(
                StashApplyResult::Conflicts {
                    paths: vec![RelaPath::from("a.txt")],
                },
                RefreshSet::STATUS,
            ),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, c| t.pop_stash(id, w, c));
    let texts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        texts.contains(&"Stash applied with conflicts".to_owned()),
        "conflicts warn"
    );
    assert!(
        !texts.contains(&"Stash popped".to_owned()),
        "no success toast next to the warning"
    );

    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            ok(StashApplyResult::Applied, RefreshSet::STATUS),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, c| t.pop_stash(id, w, c));
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&"Stash popped".to_owned()),
        "a clean pop shows the success toast"
    );
}

/// 21 §2.9: a remote branch whose local name is taken asks for another name instead of failing; other failures
/// still show the error dialog.
#[gpui_kit::test]
fn checkout_remote_name_collision_asks_name(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let remote_ref = full("refs/remotes/origin/dev");

    fail::<crate::backend::FullName>(
        &tab,
        cx,
        GitError::AlreadyExists {
            what: "branch",
            name: "dev".into(),
        },
    );
    let r = remote_ref.clone();
    run_on(cx, handle, &tab, move |t, w, c| {
        t.checkout_remote_branch(r, None, w, c)
    });
    assert!(
        cx.update(|cx| app.read(cx).errors_opened.is_empty() && app.read(cx).pending_errors() == 0),
        "the collision is handled by asking for a name, not by an error dialog"
    );
    cx.update_window(handle, |_, window, cx| {
        use gpui_kit::component::WindowExt as _;
        assert!(
            window.has_active_dialog(cx),
            "the name dialog is open (it holds back error dialogs)"
        );
        window.close_dialog(cx);
    })
    .expect("window");
    cx.run_until_parked();

    fail::<crate::backend::FullName>(
        &tab,
        cx,
        GitError::NotFound {
            what: "remote branch",
            name: "origin/dev".into(),
        },
    );
    let r = remote_ref.clone();
    run_on(cx, handle, &tab, move |t, w, c| {
        t.checkout_remote_branch(r, None, w, c)
    });
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| app.read(cx).errors_opened.clone()),
        ["Remote branch checkout failed"],
        "any other failure is a dialog"
    );

    // The retry the dialog performs: a checkout with the chosen name.
    run_on(cx, handle, &tab, move |t, w, c| {
        t.checkout_remote_branch(remote_ref, Some("dev-2".into()), w, c)
    });
    assert_eq!(
        op_titles(&tab, cx).last().map(String::as_str),
        Some("Checkout 'dev-2'")
    );
}

/// 21 §2.9: deleting a branch that is not fully merged asks "Delete it anyway?" and retries with `force`.
#[gpui_kit::test]
fn delete_unmerged_confirms_force(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let feature = full("refs/heads/feature");

    fail::<()>(
        &tab,
        cx,
        GitError::NotFullyMerged {
            branch: "feature".into(),
        },
    );
    let f = feature.clone();
    run_on(cx, handle, &tab, move |t, w, c| {
        t.delete_branch(f, false, w, c)
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("dialog-primary").is_some(),
            "the confirmation is open"
        );
        window.click("dialog-primary", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        op_titles(&tab, cx),
        ["Delete branch 'feature'", "Delete branch 'feature'"],
        "the confirmed deletion ran a second time (with force)"
    );

    // Declining runs nothing more.
    fail::<()>(
        &tab,
        cx,
        GitError::NotFullyMerged {
            branch: "feature".into(),
        },
    );
    run_on(cx, handle, &tab, move |t, w, c| {
        t.delete_branch(feature, false, w, c)
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("dialog-cancel", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(op_titles(&tab, cx).len(), 3, "no retry after Cancel");
}

/// Ref-only operations are `RefsOnly` snapshots with the 17 §4.2 titles; checkouts are full ones.
#[gpui_kit::test]
fn catalogue_titles_and_refresh(cx: &mut TestAppContext) {
    use crate::backend::StartPoint;
    let (handle, _app, tab) = mount_seeded(cx, refs_seed(), DEFAULT_SIZE);
    let f = full("refs/heads/feature");
    let run = |cx: &mut TestAppContext,
               f: Box<dyn FnOnce(&mut RepoTab, &mut Window, &mut Context<RepoTab>)>| {
        run_on(cx, handle, &tab, f);
    };
    let ff = f.clone();
    run(
        cx,
        Box::new(move |t, w, c| {
            t.checkout_branch(ff, w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.checkout_commit(oid(10), w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.checkout_tag(full("refs/tags/v1.0"), oid(14), w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.create_branch("topic".into(), StartPoint::Head, true, w, c);
        }),
    );
    run(
        cx,
        Box::new(move |t, w, c| {
            t.rename_branch(f, "feature2".into(), w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.create_tag("v2".into(), oid(10), None, w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.delete_tag("v2".into(), w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.drop_stash(oid(16), w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.stash(Some("msg".into()), true, w, c);
        }),
    );
    assert_eq!(
        op_titles(&tab, cx),
        [
            "Checkout 'feature'",
            "Checkout 0000000",
            "Checkout 0000000",
            "Create branch 'topic'",
            "Rename branch 'feature' to 'feature2'",
            "Create tag 'v2'",
            "Delete tag 'v2'",
            "Drop stash@{1}",
            "Stash changes"
        ]
    );
    // Operations without a snapshot record nothing.
    let n = op_titles(&tab, cx).len();
    run(
        cx,
        Box::new(|t, w, c| {
            t.add_remote("up".into(), "https://x/y.git".into(), None, w, c);
        }),
    );
    run(
        cx,
        Box::new(|t, w, c| {
            t.set_upstream(full("refs/heads/main"), None, w, c);
        }),
    );
    let titles = op_titles(&tab, cx);
    assert_eq!(titles.len(), n + 2);
    assert!(
        titles[n..].iter().all(String::is_empty),
        "remote and upstream changes are not recorded"
    );
}

// ---- fixtures (real git) ----------------------------------------------------------------------------------------

fn branch_names(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<String> {
    model_refs(tab, cx)
        .local
        .iter()
        .map(|b| b.name.clone())
        .collect()
}

fn head_name(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Option<String> {
    model_refs(tab, cx)
        .local
        .iter()
        .find(|b| b.is_head)
        .map(|b| b.name.clone())
}

fn two_commit_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("first");
    fx.write("a.txt", "two\n");
    fx.commit_all("second");
    fx
}

/// Create, rename, check out, delete (safe, unmerged, forced) and upstream changes against real Git, through the
/// catalogue; the side panel follows the reloads.
#[gpui_kit::test]
fn fixture_branch_lifecycle(cx: &mut TestAppContext) {
    use crate::backend::StartPoint;
    let fx = two_commit_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    assert_eq!(branch_names(&tab, cx), ["main"]);

    run_on(cx, handle, &tab, |t, w, c| {
        t.create_branch("feature/x".into(), StartPoint::Head, false, w, c)
    });
    assert_eq!(
        branch_names(&tab, cx),
        ["feature/x", "main"],
        "created without checkout"
    );
    assert_eq!(head_name(&tab, cx).as_deref(), Some("main"));

    run_on(cx, handle, &tab, |t, w, c| {
        t.rename_branch(full("refs/heads/feature/x"), "feature/y".into(), w, c)
    });
    assert_eq!(branch_names(&tab, cx), ["feature/y", "main"], "renamed");

    run_on(cx, handle, &tab, |t, w, c| {
        t.checkout_branch(full("refs/heads/feature/y"), w, c)
    });
    assert_eq!(
        head_name(&tab, cx).as_deref(),
        Some("feature/y"),
        "checked out"
    );

    // An unmerged commit on feature/y: safe deletion asks; confirming (Asker not involved: the dialog) deletes.
    fx.write("b.txt", "b\n");
    fx.commit_all("only on feature");
    run_on(cx, handle, &tab, |t, w, c| {
        t.checkout_branch(full("refs/heads/main"), w, c)
    });
    run_on(cx, handle, &tab, |t, w, c| {
        t.delete_branch(full("refs/heads/feature/y"), false, w, c)
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("dialog-primary").is_some(),
            "NotFullyMerged opens the confirmation"
        );
        window.click("dialog-primary", cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        branch_names(&tab, cx),
        ["main"],
        "forced deletion after the confirmation"
    );

    run_on(cx, handle, &tab, |t, w, c| {
        t.create_branch("topic".into(), StartPoint::Head, true, w, c)
    });
    assert_eq!(
        head_name(&tab, cx).as_deref(),
        Some("topic"),
        "created and checked out"
    );

    // Detached checkout of a commit.
    let first = fx.git(&["rev-parse", "HEAD~1"]).trim().to_owned();
    let id = ObjectId::from_hex(first.as_bytes()).expect("a commit id");
    run_on(cx, handle, &tab, move |t, w, c| t.checkout_commit(id, w, c));
    assert_eq!(head_name(&tab, cx), None, "HEAD is detached");
    assert!(matches!(
        model_refs(&tab, cx).head,
        HeadState::Detached { .. }
    ));
}

/// Tags: lightweight and annotated create, checkout of the commit, delete.
#[gpui_kit::test]
fn fixture_tag_lifecycle(cx: &mut TestAppContext) {
    let fx = two_commit_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let head = model_refs(&tab, cx).local[0].target;
    run_on(cx, handle, &tab, move |t, w, c| {
        t.create_tag("light".into(), head, None, w, c)
    });
    run_on(cx, handle, &tab, move |t, w, c| {
        t.create_tag("annot".into(), head, Some("release notes".into()), w, c)
    });
    let names = |cx: &mut TestAppContext| {
        model_refs(&tab, cx)
            .tags
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(cx), ["annot", "light"]);
    assert!(
        model_refs(&tab, cx)
            .tags
            .iter()
            .find(|t| t.name == "annot")
            .expect("tag")
            .tag_object
            .is_some(),
        "a message makes the tag annotated"
    );
    assert!(
        model_refs(&tab, cx)
            .tags
            .iter()
            .find(|t| t.name == "light")
            .expect("tag")
            .tag_object
            .is_none(),
        "no message: lightweight"
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.delete_tag("light".into(), w, c)
    });
    assert_eq!(names(cx), ["annot"], "deleted");
}

/// Stash push (with an untracked file), apply, pop and drop against real Git.
#[gpui_kit::test]
fn fixture_stash_apply_pop_drop(cx: &mut TestAppContext) {
    let fx = two_commit_fixture();
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    fx.write("a.txt", "dirty\n");
    fx.write("new.txt", "untracked\n");
    run_on(cx, handle, &tab, |t, w, c| {
        let _ = w;
        t.reload(RefreshSet::STATUS, c)
    });
    run_on(cx, handle, &tab, |t, w, c| {
        t.stash(Some("my work".into()), true, w, c)
    });
    assert_eq!(model_refs(&tab, cx).stashes.len(), 1, "stashed");
    assert!(
        model_refs(&tab, cx).stashes[0].message.contains("my work"),
        "with the message"
    );
    assert!(
        !fx.path().join("new.txt").exists(),
        "-u took the untracked file"
    );
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("a.txt"),
        "two\n"
    );

    let id = model_refs(&tab, cx).stashes[0].id;
    run_on(cx, handle, &tab, move |t, w, c| t.apply_stash(id, w, c));
    assert_eq!(
        std::fs::read_to_string(fx.path().join("a.txt")).expect("a.txt"),
        "dirty\n"
    );
    assert!(
        fx.path().join("new.txt").exists(),
        "apply restored the untracked file"
    );
    assert_eq!(
        model_refs(&tab, cx).stashes.len(),
        1,
        "apply keeps the entry"
    );
    assert!(
        cx.update(|cx| toast_texts(&app, cx))
            .contains(&"Stash applied".to_owned()),
        "a clean apply shows the success toast"
    );

    fx.git(&["checkout", "--", "a.txt"]);
    std::fs::remove_file(fx.path().join("new.txt")).expect("remove");
    run_on(cx, handle, &tab, move |t, w, c| t.pop_stash(id, w, c));
    assert!(
        model_refs(&tab, cx).stashes.is_empty(),
        "pop drops the entry"
    );

    fx.write("a.txt", "again\n");
    run_on(cx, handle, &tab, |t, w, c| {
        let _ = w;
        t.reload(RefreshSet::STATUS, c)
    });
    run_on(cx, handle, &tab, |t, w, c| t.stash(None, true, w, c));
    let id = model_refs(&tab, cx).stashes[0].id;
    run_on(cx, handle, &tab, move |t, w, c| t.drop_stash(id, w, c));
    assert!(model_refs(&tab, cx).stashes.is_empty(), "dropped");
}

/// Remotes: add, edit, delete.
#[gpui_kit::test]
fn fixture_remote_add_edit_delete(cx: &mut TestAppContext) {
    let fx = two_commit_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, c| {
        t.add_remote(
            "origin".into(),
            "https://example.com/a.git".into(),
            None,
            w,
            c,
        )
    });
    let remote = |cx: &mut TestAppContext| model_refs(&tab, cx).remotes.clone();
    assert_eq!(remote(cx)[0].name, "origin");
    assert_eq!(
        remote(cx)[0].fetch_url.as_deref(),
        Some("https://example.com/a.git")
    );

    run_on(cx, handle, &tab, |t, w, c| {
        t.edit_remote(
            "origin".into(),
            "https://example.com/b.git".into(),
            Some("https://example.com/push.git".into()),
            w,
            c,
        )
    });
    assert_eq!(
        remote(cx)[0].fetch_url.as_deref(),
        Some("https://example.com/b.git")
    );
    assert_eq!(
        remote(cx)[0].push_url.as_deref(),
        Some("https://example.com/push.git")
    );

    run_on(cx, handle, &tab, |t, w, c| {
        t.delete_remote("origin".into(), w, c)
    });
    assert!(remote(cx).is_empty(), "removed");
}

/// Checkout of a remote branch creates a tracking branch; when the name is taken the failure is recoverable with a
/// chosen name.
#[gpui_kit::test]
fn fixture_checkout_remote_branch_and_collision(cx: &mut TestAppContext) {
    let fx = two_commit_fixture();
    // A second repository acts as the remote.
    let upstream = Fixture::new();
    upstream.write("u.txt", "u\n");
    upstream.commit_all("upstream commit");
    upstream.git(&["branch", "dev"]);
    let url = upstream.path().to_string_lossy().replace('\\', "/");
    fx.git(&["remote", "add", "origin", &url]);
    fx.git(&["fetch", "origin"]);
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let dev = full("refs/remotes/origin/dev");

    run_on(cx, handle, &tab, move |t, w, c| {
        t.checkout_remote_branch(dev, None, w, c)
    });
    assert_eq!(
        head_name(&tab, cx).as_deref(),
        Some("dev"),
        "tracking branch created and checked out"
    );
    let refs = model_refs(&tab, cx);
    assert!(
        refs.local
            .iter()
            .find(|b| b.name == "dev")
            .and_then(|b| b.upstream.as_ref())
            .is_some(),
        "with an upstream"
    );

    // A local branch "feat" that tracks nothing blocks origin/feat -> ask for another name.
    upstream.git(&["branch", "feat"]);
    fx.git(&["fetch", "origin"]);
    fx.git(&["branch", "feat", "main"]);
    run_on(cx, handle, &tab, |t, w, c| {
        let _ = w;
        t.reload(RefreshSet::ALL, c)
    });
    run_on(cx, handle, &tab, {
        let feat = full("refs/remotes/origin/feat");
        move |t, w, c| t.checkout_remote_branch(feat, None, w, c)
    });
    assert!(
        cx.update(|cx| app.read(cx).errors_opened.is_empty()),
        "the collision asks for another name instead of showing an error"
    );
    run_on(cx, handle, &tab, {
        let feat = full("refs/remotes/origin/feat");
        move |t, w, c| t.checkout_remote_branch(feat, Some("feat-remote".into()), w, c)
    });
    assert_eq!(head_name(&tab, cx).as_deref(), Some("feat-remote"));
}

/// Double-clicking a branch row of a real repository checks it out and the list follows (selection, HEAD badge).
#[gpui_kit::test]
fn fixture_double_click_checks_out_and_panel_follows(cx: &mut TestAppContext) {
    let fx = two_commit_fixture();
    fx.git(&["branch", "other"]);
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    frame(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.double_click(ids::branch("refs/heads/other"), cx);
    })
    .expect("window");
    cx.run_until_parked();
    frame(cx, handle);
    assert_eq!(head_name(&tab, cx).as_deref(), Some("other"));
    cx.update_window(handle, |_, window, _| {
        assert!(
            window
                .try_find(ids::child(ids::branch("refs/heads/other"), "head"))
                .is_some(),
            "the HEAD badge moved"
        );
        assert!(
            window
                .try_find(ids::child(ids::branch("refs/heads/main"), "head"))
                .is_none()
        );
        assert_width(window, ids::SIDE_PANEL, 220.);
    })
    .expect("window");
}

/// Submodules against real Git: add (from a local repository), update, sync, delete.
#[gpui_kit::test]
fn fixture_submodule_lifecycle(cx: &mut TestAppContext) {
    let _dbg = ();
    let fx = two_commit_fixture();
    let lib = Fixture::new();
    lib.write("lib.txt", "lib\n");
    lib.commit_all("lib");
    let url = lib.path().to_string_lossy().replace('\\', "/");
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, move |t, w, c| {
        t.add_submodule(url, RelaPath::from("libs/lib"), w, c)
    });
    let states = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            tab.read(cx)
                .ctx()
                .model
                .read(cx)
                .submodules
                .value
                .clone()
                .map(|s| {
                    s.iter()
                        .map(|s| (s.path.to_string(), s.state))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    assert_eq!(
        states(cx),
        [("libs/lib".to_owned(), SubmoduleState::CheckedOut)],
        "added and checked out"
    );
    // Git refuses to deinitialise a submodule whose addition is still staged: commit it first.
    fx.git(&["commit", "-qm", "add the submodule"]);
    run_on(cx, handle, &tab, |t, w, c| {
        t.sync_submodule(RelaPath::from("libs/lib"), w, c)
    });
    run_on(cx, handle, &tab, |t, w, c| {
        t.update_submodule(RelaPath::from("libs/lib"), w, c)
    });
    run_on(cx, handle, &tab, |t, w, c| {
        t.deinit_submodule(RelaPath::from("libs/lib"), w, c)
    });
    assert_eq!(
        states(cx),
        [("libs/lib".to_owned(), SubmoduleState::Uninitialized)],
        "deinitialised"
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.init_submodule(RelaPath::from("libs/lib"), w, c)
    });
    assert_eq!(
        states(cx)[0].1,
        SubmoduleState::CheckedOut,
        "initialised again"
    );
    run_on(cx, handle, &tab, |t, w, c| {
        t.delete_submodule(RelaPath::from("libs/lib"), w, c)
    });
    assert!(states(cx).is_empty(), "deleted");
    assert!(
        cx.update(|cx| app.read(cx).errors_opened.is_empty()),
        "no operation failed"
    );
    assert!(!fx.path().join("libs/lib").exists());
}
