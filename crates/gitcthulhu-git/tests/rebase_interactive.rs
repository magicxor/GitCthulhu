//! Tests of the interactive rebase and the editor helper (spec §5, WP-B7).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, GitError, GitErrorKind, NoPrompter, OpContext, RebaseAction, RebaseOptions,
    RebaseOutcome, RebasePlan, RebaseStopReason, Repo, TodoAction, TodoLine, rebase,
};
use support::{TestEnv, TestRepo};

fn ctx(t: &TestRepo) -> OpContext {
    t.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn rev(t: &TestRepo, spec: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(t.git(&["rev-parse", spec]).as_bytes()).expect("rev-parse prints an id")
}

fn start(t: &TestRepo, repo: &Repo, plan: &RebasePlan) -> Result<RebaseOutcome, GitError> {
    let mut guard = repo.lock_exclusive();
    rebase::start_interactive(
        repo,
        guard.write_permission(),
        &ctx(t),
        plan,
        &RebaseOptions::default(),
    )
    .map(|o| o.value)
}

fn plan_dir(t: &TestRepo) -> std::path::PathBuf {
    t.path.join(".git").join("gitcthulhu").join("rebase")
}

fn rebase_dir_exists(t: &TestRepo) -> bool {
    t.path.join(".git").join("rebase-merge").exists()
}

/// `base` then three commits touching separate files: `one`, `two`, `three`.
fn linear(t: &TestRepo) -> gix::ObjectId {
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    for (file, msg) in [("f1.txt", "one"), ("f2.txt", "two"), ("f3.txt", "three")] {
        t.write(file, "x\n");
        t.commit_all(msg);
    }
    base
}

fn subjects(t: &TestRepo, base: gix::ObjectId) -> Vec<String> {
    t.git(&["log", "--format=%s", "--reverse", &format!("{base}..HEAD")])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn reword_via_editor_helper() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Reword;
    plan.steps[1].new_message = Some("reworded two\n\nwith a body".into());
    let outcome = start(&t, &repo, &plan).expect("reword rebase");
    assert!(
        matches!(outcome, RebaseOutcome::Completed { .. }),
        "completed: {outcome:?}"
    );
    assert_eq!(
        t.git(&["log", "--format=%B", "-1", "HEAD~1"]),
        "reworded two\n\nwith a body",
        "the GUI's message reached the commit through the editor helper"
    );
    assert_eq!(
        subjects(&t, base),
        ["one", "reworded two", "three"],
        "others untouched"
    );
    assert!(
        !plan_dir(&t).exists(),
        "the plan directory is removed after completion"
    );
}

#[test]
fn squash_with_custom_message() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Squash;
    plan.steps[1].new_message = Some("one and two\n\ncombined".into());
    start(&t, &repo, &plan).expect("squash rebase");
    assert_eq!(
        subjects(&t, base),
        ["one and two", "three"],
        "two commits are left"
    );
    assert_eq!(
        t.git(&["log", "--format=%B", "-1", "HEAD~1"]),
        "one and two\n\ncombined",
        "the custom message replaced the combined default"
    );
    assert!(
        t.path.join("f1.txt").exists() && t.path.join("f2.txt").exists(),
        "both changes are kept"
    );
}

#[test]
fn reword_followed_by_fixup_keeps_the_new_message() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[0].action = RebaseAction::Reword;
    plan.steps[0].new_message = Some("one reworded\n\nbody".into());
    plan.steps[1].action = RebaseAction::Fixup;
    start(&t, &repo, &plan).expect("reword + fixup rebase");
    assert_eq!(
        subjects(&t, base),
        ["one reworded", "three"],
        "the fixup melded into the reworded commit"
    );
    assert_eq!(
        t.git(&["log", "--format=%B", "-1", "HEAD~1"]),
        "one reworded\n\nbody",
        "Git asks for the message at the reword step, not at the final fixup"
    );
}

#[test]
fn fixup_group_message_on_a_pick_is_applied() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Fixup;
    plan.steps[1].new_message = Some("one and two".into());
    start(&t, &repo, &plan).expect("pick + fixup rebase");
    assert_eq!(
        subjects(&t, base),
        ["one and two", "three"],
        "the group's message is used although Git never asks for a pick + fixup message by itself"
    );
    assert!(
        t.path.join("f2.txt").exists(),
        "the fixup's changes are kept"
    );
}

#[test]
fn empty_plan_completes_without_changes() {
    let env = TestEnv::new();
    let t = env.repo("r");
    linear(&t);
    let head = rev(&t, "HEAD");
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, Some(head)).expect("plan with base = HEAD");
    assert!(plan.steps.is_empty(), "nothing between HEAD and itself");
    let outcome = start(&t, &repo, &plan).expect("an empty plan is not an error");
    match outcome {
        RebaseOutcome::Completed { new_head } => assert_eq!(new_head, head, "HEAD unchanged"),
        other => panic!("expected Completed, got {other:?}"),
    }
    assert!(!rebase_dir_exists(&t), "no rebase is left in progress");
}

#[test]
fn squash_default_message() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Squash;
    start(&t, &repo, &plan).expect("squash rebase");
    let message = t.git(&["log", "--format=%B", "-1", "HEAD~1"]);
    assert!(
        message.contains("one") && message.contains("two"),
        "Git's combined message has both: {message:?}"
    );
    assert!(
        !message.contains('#'),
        "comment lines of the combined message are stripped: {message:?}"
    );
}

#[test]
fn fixup() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Fixup;
    start(&t, &repo, &plan).expect("fixup rebase");
    assert_eq!(
        subjects(&t, base),
        ["one", "three"],
        "the fixup commit melded away"
    );
    assert_eq!(
        t.git(&["log", "--format=%B", "-1", "HEAD~1"]),
        "one",
        "the message of the first commit is kept"
    );
    assert!(t.path.join("f2.txt").exists(), "its changes are kept");
}

#[test]
fn drop_explicit_line_with_missing_commits_check_error() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    t.git(&["config", "rebase.missingCommitsCheck", "error"]);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Drop;
    let outcome = start(&t, &repo, &plan).expect("an explicit drop line is not a missing commit");
    assert!(
        matches!(outcome, RebaseOutcome::Completed { .. }),
        "completed: {outcome:?}"
    );
    assert_eq!(
        subjects(&t, base),
        ["one", "three"],
        "the dropped commit is gone"
    );
    assert!(!t.path.join("f2.txt").exists(), "with its changes");
}

#[test]
fn reorder() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps.swap(1, 2);
    start(&t, &repo, &plan).expect("reorder rebase");
    assert_eq!(
        subjects(&t, base),
        ["one", "three", "two"],
        "order follows the plan"
    );
}

#[test]
fn edit_stops_with_amend_then_continue() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Edit;
    let outcome = start(&t, &repo, &plan).expect("edit rebase stops");
    match outcome {
        RebaseOutcome::Stopped {
            reason,
            at,
            conflicts,
        } => {
            assert_eq!(reason, RebaseStopReason::Edit, "an edit step");
            assert!(at.is_some(), "the stopped commit is known");
            assert!(conflicts.is_empty(), "no conflicts");
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    assert!(
        plan_dir(&t).exists(),
        "the plan directory is kept while the rebase runs"
    );
    assert!(
        plan_dir(&t).join("plan.json").is_file(),
        "plan.json marks the rebase as started by us (RebaseState::started_by_us) in every build"
    );

    t.git(&["commit", "-q", "--amend", "-m", "two amended"]);
    let mut guard = repo.lock_exclusive();
    let out = rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), None)
        .expect("continue after amending");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
    assert_eq!(
        subjects(&t, base),
        ["one", "two amended", "three"],
        "the amend is kept"
    );
    assert!(
        !plan_dir(&t).exists(),
        "the plan directory is removed at the end"
    );
}

#[test]
fn conflict_stop_then_continue_with_message_for_reword() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    t.write("x.txt", "x\n");
    t.commit_all("one");
    t.write("a.txt", "1\n");
    t.commit_all("two");
    t.write("a.txt", "2\n");
    t.commit_all("three");
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    // "three" depends on "two": without it, replaying "three" conflicts on a.txt.
    plan.steps[1].action = RebaseAction::Drop;
    plan.steps[2].action = RebaseAction::Reword;
    plan.steps[2].new_message = Some("ignored: overridden by continue".into());
    let outcome = start(&t, &repo, &plan).expect("a conflict is a stop");
    match outcome {
        RebaseOutcome::Stopped {
            reason, conflicts, ..
        } => {
            assert_eq!(
                reason,
                RebaseStopReason::Conflict,
                "conflict on the reword step"
            );
            assert_eq!(
                conflicts,
                [bstr::BString::from("a.txt")],
                "the conflicted path"
            );
        }
        other => panic!("expected Stopped, got {other:?}"),
    }
    t.write("a.txt", "resolved\n");
    t.git(&["add", "a.txt"]);
    let mut guard = repo.lock_exclusive();
    let out = rebase::continue_rebase(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        Some("final message\n\nfrom the dialog"),
    )
    .expect("continue with a message");
    assert!(
        matches!(out.value, RebaseOutcome::Completed { .. }),
        "completed: {:?}",
        out.value
    );
    assert_eq!(
        t.git(&["log", "--format=%B", "-1"]),
        "final message\n\nfrom the dialog",
        "the message given to continue is the commit message"
    );
}

#[test]
fn stale_plan_head_moved() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    t.write("late.txt", "late\n");
    let moved = t.commit_all("late commit");
    let result = start(&t, &repo, &plan);
    match result {
        Err(GitError::RebasePlanStale { detail }) => {
            assert_eq!(detail, "HEAD moved", "the stale reason")
        }
        other => panic!("expected RebasePlanStale, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), moved, "nothing happened");
    assert!(!plan_dir(&t).exists(), "no plan directory was left behind");

    let plan = rebase::plan_interactive(&repo, Some(base)).expect("fresh plan on main");
    t.git(&["switch", "-q", "-c", "other"]);
    assert!(
        matches!(
            start(&t, &repo, &plan),
            Err(GitError::RebasePlanStale { .. })
        ),
        "another branch at the same commit is not the planned branch"
    );
    assert_eq!(rev(&t, "main"), moved, "main was not rewritten");
}

#[test]
fn stale_plan_mismatch_aborts_cleanly() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let head = rev(&t, "HEAD");
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    // HEAD is unchanged, but the plan names a commit that Git will not list.
    let mut extra = plan.steps[0].clone();
    extra.commit = base;
    plan.steps.insert(0, extra);
    let result = start(&t, &repo, &plan);
    match result {
        Err(GitError::RebasePlanStale { detail }) => assert_eq!(
            detail, "Git would rebase a different set of commits",
            "the helper refused the plan"
        ),
        other => panic!("expected RebasePlanStale, got {other:?}"),
    }
    assert_eq!(rev(&t, "HEAD"), head, "HEAD is untouched");
    assert!(!rebase_dir_exists(&t), "no rebase is left in progress");
    assert!(!plan_dir(&t).exists(), "the plan directory is removed");
    assert_eq!(
        t.git(&["branch", "--show-current"]),
        "main",
        "still on the branch"
    );
}

#[test]
fn update_refs_config_not_applied() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    t.git(&["config", "rebase.updateRefs", "true"]);
    t.git(&["branch", "mid", "HEAD~1"]);
    let mid = rev(&t, "mid");
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[0].action = RebaseAction::Reword;
    plan.steps[0].new_message = Some("one reworded".into());
    start(&t, &repo, &plan).expect("reword rebase");
    assert_eq!(
        rev(&t, "mid"),
        mid,
        "a branch inside the range is not moved by rebase.updateRefs"
    );
}

#[test]
fn rebase_autosquash_config_ignored() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a.txt", "0\n");
    let base = t.commit_all("base");
    for (file, msg) in [
        ("f1.txt", "first"),
        ("f2.txt", "second"),
        ("f3.txt", "fixup! first"),
    ] {
        t.write(file, "x\n");
        t.commit_all(msg);
    }
    t.git(&["config", "rebase.autoSquash", "true"]);
    let repo = t.open();
    let plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    start(&t, &repo, &plan).expect("rebase");
    assert_eq!(
        subjects(&t, base),
        ["first", "second", "fixup! first"],
        "the plan's order wins over rebase.autoSquash"
    );
}

#[test]
fn plan_dir_cleaned_after_terminal_finish() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[1].action = RebaseAction::Edit;
    start(&t, &repo, &plan).expect("edit rebase stops");
    assert!(
        plan_dir(&t).exists(),
        "the plan directory exists while stopped"
    );
    t.git(&["rebase", "--continue"]);
    assert!(
        !rebase_dir_exists(&t),
        "the user finished the rebase in a terminal"
    );

    let mut guard = repo.lock_exclusive();
    let result = rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), None);
    assert!(
        matches!(result, Err(GitError::NoOperationInProgress { .. })),
        "nothing to continue: {result:?}"
    );
    assert!(
        !plan_dir(&t).exists(),
        "the stale plan directory was removed"
    );
}

#[test]
fn editor_helper_paths_with_spaces_and_quotes() {
    let env = TestEnv::new();
    let t = env.repo("a b'c");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[2].action = RebaseAction::Reword;
    plan.steps[2].new_message = Some("three reworded".into());
    plan.steps.swap(0, 1);
    let outcome = start(&t, &repo, &plan).expect("rebase in a path with a space and a quote");
    assert!(
        matches!(outcome, RebaseOutcome::Completed { .. }),
        "completed: {outcome:?}"
    );
    assert_eq!(
        subjects(&t, base),
        ["two", "one", "three reworded"],
        "both helper modes worked"
    );
}

#[test]
fn start_requires_a_clean_state_and_a_valid_plan() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");

    let mut squash_first = plan.clone();
    squash_first.steps[0].action = RebaseAction::Squash;
    assert_eq!(
        match start(&t, &repo, &squash_first) {
            Err(e) => e.kind(),
            Ok(o) => panic!("expected an error, got {o:?}"),
        },
        GitErrorKind::InvalidName,
        "squash without a preceding commit"
    );

    plan.steps[1].action = RebaseAction::Edit;
    start(&t, &repo, &plan).expect("edit rebase stops");
    let again = start(&t, &repo, &plan);
    assert!(
        matches!(again, Err(GitError::OperationInProgress { .. })),
        "a second start while one is running: {again:?}"
    );
}

#[test]
fn edit_todo_replaces_the_remaining_todo() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let base = linear(&t);
    let repo = t.open();
    let mut plan = rebase::plan_interactive(&repo, Some(base)).expect("plan");
    plan.steps[0].action = RebaseAction::Edit;
    start(&t, &repo, &plan).expect("edit rebase stops at the first commit");
    let (two, three) = (plan.steps[1].commit, plan.steps[2].commit);
    let line = |action, commit, rest: &str| TodoLine {
        action,
        commit: Some(commit),
        abbrev: None,
        rest: rest.into(),
        raw: String::new(),
    };
    let todo = vec![
        line(TodoAction::Pick, two, "two"),
        line(TodoAction::Drop, three, "three"),
    ];
    let mut guard = repo.lock_exclusive();
    rebase::edit_todo(&repo, guard.write_permission(), &ctx(&t), &todo).expect("edit the todo");
    let text = std::fs::read_to_string(
        t.path
            .join(".git")
            .join("rebase-merge")
            .join("git-rebase-todo"),
    )
    .expect("todo exists");
    assert_eq!(
        text,
        format!("pick {two} two\ndrop {three} three\n"),
        "the remaining todo was replaced"
    );
    let bad = vec![TodoLine {
        action: TodoAction::Comment,
        commit: None,
        abbrev: None,
        rest: String::new(),
        raw: "frobnicate abc".into(),
    }];
    assert!(
        rebase::edit_todo(&repo, guard.write_permission(), &ctx(&t), &bad).is_err(),
        "an unknown command is refused before anything is written"
    );
    let injected = vec![line(TodoAction::Pick, two, "two\nexec rm -rf something")];
    assert!(
        matches!(
            rebase::edit_todo(&repo, guard.write_permission(), &ctx(&t), &injected),
            Err(GitError::InvalidName { .. })
        ),
        "a line break inside a line would add an unchecked line"
    );
    let missing_commit = vec![TodoLine {
        action: TodoAction::Pick,
        commit: None,
        abbrev: None,
        rest: "two".into(),
        raw: String::new(),
    }];
    assert!(
        rebase::edit_todo(&repo, guard.write_permission(), &ctx(&t), &missing_commit).is_err(),
        "a pick needs a commit"
    );
    assert_eq!(
        std::fs::read_to_string(
            t.path
                .join(".git")
                .join("rebase-merge")
                .join("git-rebase-todo"),
        )
        .expect("todo exists"),
        text,
        "refused edits leave the todo untouched"
    );
    rebase::continue_rebase(&repo, guard.write_permission(), &ctx(&t), None)
        .expect("continue with the edited todo");
    assert_eq!(
        subjects(&t, base),
        ["one", "two"],
        "the dropped commit is gone"
    );
}
