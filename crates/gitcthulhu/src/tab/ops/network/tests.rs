use core::prelude::v1::test;

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext, Window};

use super::*;
use crate::backend::{
    OpOutcome, OplogEntry, PageRequest, PushRefStatus, PushedRefUpdate, RemoteBranch,
};
use crate::test_support::seed::{full, oid};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, mount_fixture, mount_seeded, script_op,
    toast_texts,
};

fn update(
    local: Option<&str>,
    remote_ref: &str,
    status: PushRefStatus,
    new: Option<u64>,
) -> PushedRefUpdate {
    PushedRefUpdate {
        local: local.map(full),
        remote_ref: full(remote_ref),
        old: None,
        new: new.map(oid),
        status,
        summary: String::new(),
    }
}

fn report(updates: Vec<PushedRefUpdate>) -> PushReport {
    PushReport {
        remote: "origin".to_owned(),
        url: None,
        updates,
    }
}

#[test]
fn push_rejection_marker_built_from_report() {
    let mixed = report(vec![
        update(
            Some("refs/heads/main"),
            "refs/heads/main",
            PushRefStatus::FastForward,
            Some(1),
        ),
        update(
            Some("refs/heads/feature"),
            "refs/heads/feature",
            PushRefStatus::Rejected,
            None,
        ),
        update(
            Some("refs/heads/same"),
            "refs/heads/same",
            PushRefStatus::UpToDate,
            Some(2),
        ),
    ]);
    // A rejection carries the full report: the marker keeps what reached the remote.
    let rejection = GitError::NonFastForward {
        refs: vec!["refs/heads/feature".to_owned()],
        report: mixed.clone(),
        stderr: String::new(),
    };
    let marker = marker_of(OperationKind::Push, push_title, Err(&rejection))
        .expect("the accepted update is recorded");
    assert_eq!(marker.kind, OperationKind::Push);
    assert_eq!(marker.remote, "origin");
    assert_eq!(
        marker.title, "Push main to origin/main",
        "the title names the one ref that reached the remote"
    );
    assert_eq!(
        marker.updates.len(),
        1,
        "rejected and up-to-date refs are not recorded"
    );
    assert_eq!(marker.updates[0].remote_ref, full("refs/heads/main"));
    assert_eq!(marker.updates[0].new_target, Some(oid(1)));
    assert!(!marker.updates[0].forced);

    let ok = marker_of(OperationKind::Push, push_title, Ok(&mixed)).expect("a marker");
    assert_eq!(ok.updates.len(), 1);

    let two = report(vec![
        update(
            Some("refs/heads/main"),
            "refs/heads/main",
            PushRefStatus::FastForward,
            Some(1),
        ),
        update(
            Some("refs/tags/v1"),
            "refs/tags/v1",
            PushRefStatus::New,
            Some(4),
        ),
    ]);
    let marker = marker_of(OperationKind::PushTags, push_title, Ok(&two)).expect("a marker");
    assert_eq!(marker.title, "Push to origin", "several refs: the remote");
    assert_eq!(marker.updates.len(), 2);

    let forced = report(vec![update(
        Some("refs/heads/main"),
        "refs/heads/main",
        PushRefStatus::Forced,
        Some(3),
    )]);
    let marker = marker_of(OperationKind::Push, push_title, Ok(&forced)).expect("a marker");
    assert_eq!(marker.title, "Push main to origin/main");
    assert!(marker.updates[0].forced);

    let deleted = report(vec![update(
        None,
        "refs/heads/old",
        PushRefStatus::Deleted,
        None,
    )]);
    let marker = marker_of(
        OperationKind::DeleteRemoteBranch,
        |_| "Delete origin/old".to_owned(),
        Ok(&deleted),
    )
    .expect("a deletion is recorded");
    assert_eq!(marker.updates[0].new_target, None);
    assert_eq!(marker.title, "Delete origin/old");

    let nothing = report(vec![update(
        Some("refs/heads/main"),
        "refs/heads/main",
        PushRefStatus::UpToDate,
        Some(1),
    )]);
    assert!(
        marker_of(OperationKind::Push, push_title, Ok(&nothing)).is_none(),
        "an up-to-date push leaves no marker"
    );
    let other_error = GitError::Cancelled;
    assert!(marker_of(OperationKind::Push, push_title, Err(&other_error)).is_none());
}

fn remote(name: &str) -> RemoteInfo {
    RemoteInfo {
        name: name.to_owned(),
        fetch_url: None,
        push_url: None,
        head_branch: None,
        branches: Vec::<RemoteBranch>::new(),
    }
}

#[test]
fn remote_branches_split_into_remote_and_branch() {
    let remotes = [remote("origin"), remote("team/fork"), remote("team")];
    let split = |name: &str| split_remote_branch(&remotes, &full(name));
    assert_eq!(
        split("refs/remotes/origin/feature/x"),
        Some(("origin".to_owned(), "feature/x".to_owned()))
    );
    assert_eq!(
        split("refs/remotes/team/fork/main"),
        Some(("team/fork".to_owned(), "main".to_owned())),
        "the longest known remote name wins"
    );
    assert_eq!(
        split("refs/remotes/team/main"),
        Some(("team".to_owned(), "main".to_owned()))
    );
    assert_eq!(
        split("refs/remotes/unknown/main"),
        Some(("unknown".to_owned(), "main".to_owned())),
        "an unloaded remote falls back to the first segment"
    );
    assert_eq!(split("refs/heads/main"), None, "not a remote branch");
    assert_eq!(split("refs/remotes/origin"), None);
}

#[test]
fn fetch_failures_are_summarised() {
    let mut fetched = FetchReport {
        per_remote: vec![("origin".to_owned(), Ok(())), ("fork".to_owned(), Ok(()))],
        updated_refs: 0,
    };
    assert_eq!(fetch_failure_text(&fetched), None);
    fetched.per_remote[1].1 = Err("fatal: unable to access\nmore".to_owned());
    assert_eq!(
        fetch_failure_text(&fetched).as_deref(),
        Some("Fetching fork failed: fatal: unable to access")
    );
    fetched.per_remote[0].1 = Err("fatal: nope".to_owned());
    assert_eq!(
        fetch_failure_text(&fetched).as_deref(),
        Some("Fetching failed for 2 remotes: origin, fork")
    );
}

// ----------------------------------------------------------------------------------------------------------------
// Seeded tabs (inert backend): the recovery flows ask and retry.

fn failure(error: GitError) -> Result<OpOutcome<MergeOutcomeOrRebase>, OpFailure> {
    Err(OpFailure::Git(Arc::new(error)))
}

fn pulls(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<Option<String>> {
    cx.update(|cx| {
        calls(tab, cx)
            .into_iter()
            .filter_map(|call| match call {
                TestCall::Op {
                    kind: OpKind::Pull,
                    title,
                } => Some(title),
                _ => None,
            })
            .collect()
    })
}

fn kinds(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<OpKind> {
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

fn frame(cx: &mut TestAppContext, handle: AnyWindowHandle, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        f(window, cx);
    })
    .expect("window");
    cx.run_until_parked();
}

fn noop_change_upstream() -> OpSpec<()> {
    OpSpec::new(OpKind::ChangeUpstream, |_| {
        Ok(OpOutcome {
            value: (),
            refresh: RefreshSet::BRANCHES,
            log: Default::default(),
            warnings: Vec::new(),
        })
    })
}

#[gpui_kit::test]
fn pull_strategy_required_asks_and_retries(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::PullStrategyRequired {
                stderr: "fatal: Need to specify how to reconcile divergent branches.".to_owned(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    assert_eq!(
        pulls(&tab, cx),
        [Some("Pull main".to_owned())],
        "one pull so far, recorded as `Pull {{head}}`"
    );
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(("choice-option", 0usize)).is_some(),
            "Merge"
        );
        assert!(
            window.try_find(("choice-option", 1usize)).is_some(),
            "Rebase"
        );
        assert!(window.try_find("choice-cancel").is_some(), "Cancel");
    });
    frame(cx, handle, |window, cx| {
        window.click(("choice-option", 1usize), cx)
    });
    assert_eq!(
        pulls(&tab, cx).len(),
        2,
        "the answer runs the pull again (no error dialog, no stray toast)"
    );
    cx.update(|cx| {
        let busy = tab.read(cx).busy().is_some();
        assert!(!busy, "the retry finished");
    });
}

#[gpui_kit::test]
fn pull_strategy_cancel_does_not_retry(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::PullStrategyRequired {
                stderr: String::new(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    frame(cx, handle, |window, cx| window.click("choice-cancel", cx));
    assert_eq!(
        pulls(&tab, cx).len(),
        1,
        "Cancel leaves the repository alone"
    );
}

#[gpui_kit::test]
fn no_upstream_opens_upstream_dialog(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::NoUpstream {
                branch: Some("main".to_owned()),
                stderr: String::new(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    frame(cx, handle, |_, _| ());
    // The Change-upstream dialog belongs to G10: until it exists its entry point answers with a toast.
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    let dialog = cx
        .update_window(handle, |_, window, cx| window.has_active_dialog(cx))
        .unwrap_or(false);
    assert!(
        toasts.iter().any(|t| t.contains("Change upstream")) || dialog,
        "the Change-upstream dialog was requested: {toasts:?}"
    );
    assert!(
        !toasts.iter().any(|t| t.contains("Pull failed")),
        "no generic error for a recoverable failure"
    );

    // Once the upstream is set (the dialog runs `set_upstream`) the pull runs again.
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(noop_change_upstream(), w, cx)
    });
    assert_eq!(
        kinds(&tab, cx),
        [OpKind::Pull, OpKind::ChangeUpstream, OpKind::Pull],
        "the upstream change retries the pull exactly once"
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(noop_change_upstream(), w, cx)
    });
    assert_eq!(
        kinds(&tab, cx).len(),
        4,
        "a later upstream change does not pull again"
    );
}

/// Accepting the dialog while the tab is busy with another upstream change starts nothing: the wait ends at once,
/// so that change finishing later does not retry the pull.
#[gpui_kit::test]
fn no_upstream_wait_ends_when_the_accepted_change_is_refused(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::NoUpstream {
                branch: Some("main".to_owned()),
                stderr: String::new(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    frame(cx, handle, |_, _| ());
    assert!(
        cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap_or(false),
        "the Change-upstream dialog is open"
    );
    // A slow upstream change from elsewhere keeps the tab busy (its start does not end the wait).
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(OpOutcome {
                value: (),
                refresh: RefreshSet::BRANCHES,
                log: Default::default(),
                warnings: Vec::new(),
            }),
            Duration::from_millis(300),
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(noop_change_upstream(), w, cx)
    });
    for id in [
        "upstream-remote-button",
        "upstream-remote-option-0",
        "upstream-branch-button",
        "upstream-branch-option-0",
        "dialog-primary",
    ] {
        frame(cx, handle, |window, cx| window.click(id, cx));
    }
    assert!(
        !cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx))
            .unwrap_or(true),
        "accepting closed the dialog"
    );
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(
        kinds(&tab, cx),
        [OpKind::Pull, OpKind::ChangeUpstream],
        "the refused accept left nothing waiting: the other change does not retry the pull"
    );
}

#[gpui_kit::test]
fn no_upstream_wait_ends_when_something_else_runs(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::NoUpstream {
                branch: Some("main".to_owned()),
                stderr: String::new(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    // The dialog was cancelled and the user fetched instead; a later upstream change is unrelated.
    run_on(cx, handle, &tab, |t, w, cx| t.fetch_all(w, cx));
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(noop_change_upstream(), w, cx)
    });
    assert_eq!(
        kinds(&tab, cx),
        [OpKind::Pull, OpKind::Fetch, OpKind::ChangeUpstream]
    );
}

#[gpui_kit::test]
fn push_without_upstream_asks_and_retries(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op::<OpOutcome<PushReport>>(
            &tab,
            cx,
            Err(OpFailure::Git(Arc::new(GitError::NoUpstream {
                branch: Some("main".to_owned()),
                stderr: String::new(),
            }))),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, false, w, cx)
    });
    run_on(cx, handle, &tab, |t, w, cx| {
        t.run_op(noop_change_upstream(), w, cx)
    });
    assert_eq!(
        kinds(&tab, cx),
        [OpKind::Push, OpKind::ChangeUpstream, OpKind::Push]
    );
}

#[gpui_kit::test]
fn detached_head_cannot_ask_for_an_upstream(cx: &mut TestAppContext) {
    let mut seed = RepoSeed::branchy();
    let detached = crate::backend::HeadState::Detached { target: oid(10) };
    seed.refs.head = detached.clone();
    seed.state.head = detached;
    let (handle, app, tab) = mount_seeded(cx, seed, DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            failure(GitError::NoUpstream {
                branch: None,
                stderr: String::new(),
            }),
            Duration::ZERO,
        )
    });
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("HEAD is detached")),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn pull_titles_and_remote_branch_requests(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, RepoSeed::branchy(), DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.pull_from(full("refs/remotes/origin/main"), w, cx)
    });
    assert_eq!(pulls(&tab, cx), [Some("Pull main".to_owned())]);
    let started = run_on(cx, handle, &tab, |t, w, cx| {
        t.pull_from(full("refs/heads/main"), w, cx)
    });
    assert!(!started, "a local branch is not a remote branch");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts.iter().any(|t| t.contains("is not a remote branch")),
        "{toasts:?}"
    );
    let started = run_on(cx, handle, &tab, |t, w, cx| {
        t.push_branch_to(
            full("refs/heads/main"),
            full("refs/heads/other"),
            false,
            w,
            cx,
        )
    });
    assert!(!started);
}

// ----------------------------------------------------------------------------------------------------------------
// Fixtures (real git).

/// A repository with a bare `origin` (pushed `main`) and a second clone that can publish commits.
struct Remote {
    fx: Fixture,
    remote: String,
    other: String,
}

impl Remote {
    fn new() -> Self {
        let fx = Fixture::new();
        let remote = fx
            .dir
            .path()
            .join("remote.git")
            .to_string_lossy()
            .into_owned();
        let other = fx.dir.path().join("other").to_string_lossy().into_owned();
        fx.git(&["init", "-q", "--bare", "-b", "main", &remote]);
        fx.write("a.txt", "one\n");
        fx.commit_all("init");
        fx.git(&["remote", "add", "origin", &remote]);
        fx.git(&["push", "-q", "-u", "origin", "main"]);
        fx.git(&["clone", "-q", &remote, &other]);
        Self { fx, remote, other }
    }

    /// Commits a file in the other clone and pushes it.
    fn publish(&self, file: &str) {
        std::fs::write(std::path::Path::new(&self.other).join(file), "x\n").expect("write");
        self.fx.git(&["-C", &self.other, "add", "-A"]);
        self.fx.git(&[
            "-C",
            &self.other,
            "commit",
            "-q",
            "-m",
            &format!("theirs {file}"),
        ]);
        self.fx
            .git(&["-C", &self.other, "push", "-q", "origin", "main"]);
    }

    fn remote_git(&self, args: &[&str]) -> String {
        let mut full_args = vec!["--git-dir", self.remote.as_str()];
        full_args.extend_from_slice(args);
        self.fx.git(&full_args)
    }
}

fn oplog_entries(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<OplogEntry> {
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

fn marker_titles(tab: &Entity<RepoTab>, cx: &mut TestAppContext) -> Vec<String> {
    let mut titles: Vec<String> = oplog_entries(tab, cx)
        .into_iter()
        .filter_map(|e| e.network.map(|n| n.title))
        .collect();
    titles.reverse();
    titles
}

#[gpui_kit::test]
fn fixture_pull_strategy_choice_rebases_a_diverged_branch(cx: &mut TestAppContext) {
    let r = Remote::new();
    r.publish("theirs.txt");
    r.fx.write("mine.txt", "mine\n");
    r.fx.commit_all("mine");
    let (handle, _app, tab) = mount_fixture(cx, &r.fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    cx.run_until_parked();
    frame(cx, handle, |window, _| {
        assert!(
            window.try_find(("choice-option", 1usize)).is_some(),
            "a diverged pull without a configured strategy asks"
        );
    });
    assert!(
        r.fx.git(&["log", "--format=%s"])
            .lines()
            .all(|l| l != "theirs theirs.txt"),
        "nothing was integrated before the answer"
    );
    frame(cx, handle, |window, cx| {
        window.click(("choice-option", 1usize), cx)
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    let log = r.fx.git(&["log", "--format=%s"]);
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["mine", "theirs theirs.txt", "init"],
        "Rebase replays the local commit on top of the remote one"
    );
    assert!(
        r.fx.git(&["rev-list", "--merges", "HEAD"])
            .trim()
            .is_empty(),
        "no merge commit"
    );
    assert!(
        oplog_entries(&tab, cx)
            .iter()
            .any(|e| e.title == "Pull main"),
        "the pull is recorded as `Pull main`"
    );
}

#[gpui_kit::test]
fn fixture_remembered_pull_strategy_is_written_to_the_repository(cx: &mut TestAppContext) {
    let r = Remote::new();
    r.publish("theirs.txt");
    r.fx.write("mine.txt", "mine\n");
    r.fx.commit_all("mine");
    let (handle, _app, tab) = mount_fixture(cx, &r.fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.retry_pull_with(
            PullRequest {
                target: None,
                mode: None,
            },
            PullMode::Merge,
            true,
            w,
            cx,
        )
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    assert_eq!(
        r.fx.git(&["config", "--local", "--get", "pull.rebase"])
            .trim(),
        "false",
        "\"Remember for this repository\" wrote pull.rebase"
    );
    assert_eq!(
        r.fx.git(&["rev-list", "--merges", "--count", "HEAD"])
            .trim(),
        "1",
        "the pull merged"
    );
    // The remembered strategy lets the next divergent pull run without asking.
    r.publish("more.txt");
    r.fx.write("mine2.txt", "mine\n");
    r.fx.commit_all("mine2");
    run_on(cx, handle, &tab, |t, w, cx| t.pull(None, w, cx));
    for _ in 0..30 {
        cx.run_until_parked();
    }
    frame(cx, handle, |window, _| {
        assert!(window.try_find(("choice-option", 0usize)).is_none());
    });
    assert_eq!(
        r.fx.git(&["rev-list", "--merges", "--count", "HEAD"])
            .trim(),
        "2"
    );
}

#[gpui_kit::test]
fn fixture_pull_from_and_fetch_remote(cx: &mut TestAppContext) {
    let r = Remote::new();
    let (handle, _app, tab) = mount_fixture(cx, &r.fx, DEFAULT_SIZE);
    r.publish("theirs.txt");
    run_on(cx, handle, &tab, |t, w, cx| {
        t.fetch_remote("origin".to_owned(), w, cx)
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    let behind = r.fx.git(&["rev-list", "--count", "main..origin/main"]);
    assert_eq!(behind.trim(), "1", "the fetch updated origin/main only");
    assert_eq!(
        r.fx.git(&["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "the branch itself did not move"
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.pull_from(full("refs/remotes/origin/main"), w, cx)
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    assert_eq!(
        r.fx.git(&["log", "-1", "--format=%s"]).trim(),
        "theirs theirs.txt",
        "pulling the remote branch fast-forwarded"
    );
}

#[gpui_kit::test]
fn fixture_push_family_records_markers(cx: &mut TestAppContext) {
    let r = Remote::new();
    r.fx.git(&["tag", "v1"]);
    r.fx.git(&["branch", "feature"]);
    let (handle, _app, tab) = mount_fixture(cx, &r.fx, DEFAULT_SIZE);
    let settle = |cx: &mut TestAppContext| {
        for _ in 0..30 {
            cx.run_until_parked();
        }
    };

    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_branch_to(
            full("refs/heads/feature"),
            full("refs/remotes/origin/feature"),
            false,
            w,
            cx,
        )
    });
    settle(cx);
    assert!(
        r.remote_git(&["branch", "--list", "feature"])
            .contains("feature")
    );

    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_tag("origin".to_owned(), "v1".to_owned(), w, cx)
    });
    settle(cx);
    assert!(r.remote_git(&["tag", "--list", "v1"]).contains("v1"));

    run_on(cx, handle, &tab, |t, w, cx| {
        t.delete_remote_tag("origin".to_owned(), "v1".to_owned(), w, cx)
    });
    settle(cx);
    assert!(r.remote_git(&["tag", "--list", "v1"]).trim().is_empty());

    run_on(cx, handle, &tab, |t, w, cx| {
        t.delete_remote_branch("origin".to_owned(), "feature".to_owned(), w, cx)
    });
    settle(cx);
    assert!(
        r.remote_git(&["branch", "--list", "feature"])
            .trim()
            .is_empty()
    );

    assert_eq!(
        marker_titles(&tab, cx),
        [
            "Push feature to origin/feature",
            "Push tag 'v1' to origin",
            "Delete tag 'v1' on origin",
            "Delete origin/feature",
        ]
    );

    // Pushing what is already there records nothing.
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, false, w, cx)
    });
    settle(cx);
    assert_eq!(
        marker_titles(&tab, cx).len(),
        4,
        "an up-to-date push leaves no marker"
    );
}

#[gpui_kit::test]
fn fixture_force_push_after_amend_uses_lease(cx: &mut TestAppContext) {
    let r = Remote::new();
    r.fx.write("a.txt", "amended\n");
    r.fx.git(&["add", "-A"]);
    r.fx.git(&["commit", "-q", "--amend", "-m", "init amended"]);
    let (handle, _app, tab) = mount_fixture(cx, &r.fx, DEFAULT_SIZE);
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(false, false, w, cx)
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    assert_eq!(
        r.remote_git(&["log", "-1", "--format=%s", "main"]).trim(),
        "init",
        "a plain push of a rewritten branch is rejected"
    );
    run_on(cx, handle, &tab, |t, w, cx| {
        t.push_current(true, false, w, cx)
    });
    for _ in 0..30 {
        cx.run_until_parked();
    }
    assert_eq!(
        r.remote_git(&["log", "-1", "--format=%s", "main"]).trim(),
        "init amended",
        "Force push (with lease) overwrites it"
    );
    assert!(
        marker_titles(&tab, cx)
            .iter()
            .any(|t| t == "Push main to origin/main"),
        "the forced push is recorded"
    );
}
