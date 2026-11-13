//! Tests of `state::repo_state`, `state::has_conflicts` and `state::parse_todo_line` (spec §1.10, §2.5).

mod support;

use gitcthulhu_git::{
    FixupFlag, HeadState, InProgress, RebaseBackend, RebaseStopReason, Repo, RepoState,
    SequencerKind, TodoAction,
    state::{has_conflicts, parse_todo_line, repo_state},
};
use support::{TestEnv, TestRepo};

fn state(repo: &TestRepo) -> RepoState {
    repo_state(&repo.open()).expect("repo_state succeeds")
}

/// `main` and `feature` both change `f.txt` after a common base; HEAD is on `main`. `feature` has two commits
/// (`f.txt`, then `g.txt`), `main` has one.
fn diverged(env: &TestEnv) -> TestRepo {
    let repo = env.repo("repo");
    repo.write("f.txt", "base\n");
    repo.write("keep.txt", "keep\n");
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("f.txt", "feature\n");
    repo.commit_all("feature change");
    repo.write("g.txt", "g\n");
    repo.commit_all("feature second");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("f.txt", "main\n");
    repo.commit_all("main change");
    repo
}

#[test]
fn safe_branch() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    let state = state(&repo);
    match &state.head {
        HeadState::Branch { name, target } => {
            assert_eq!(name.as_bstr(), "refs/heads/main", "branch name");
            assert_eq!(target.to_string(), tip, "branch target");
        }
        other => panic!("expected a branch, got {other:?}"),
    }
    assert_eq!(state.in_progress, None, "nothing in progress");
    assert!(
        state.merge.is_none() && state.sequencer.is_none() && state.rebase.is_none(),
        "no details"
    );
    assert!(state.bisect.is_none(), "no bisect");
    assert_eq!(state.conflicted_paths, 0, "no conflicts");
    assert!(
        !state.flags.bare && !state.flags.linked_worktree && !state.flags.shallow,
        "plain repository"
    );
    assert!(
        !has_conflicts(&repo.open()).expect("has_conflicts"),
        "no conflicts"
    );
}

#[test]
fn detached() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git(&["checkout", "-q", "--detach", "feature"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    match state(&repo).head {
        HeadState::Detached { target } => assert_eq!(target.to_string(), tip, "target"),
        other => panic!("expected detached, got {other:?}"),
    }
}

#[test]
fn unborn() {
    let env = TestEnv::new();
    let repo = env.repo("repo");
    match state(&repo).head {
        HeadState::Unborn { name } => {
            assert_eq!(name.as_bstr(), "refs/heads/main", "unborn branch")
        }
        other => panic!("expected unborn, got {other:?}"),
    }
}

#[test]
fn merging_with_conflicts() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let feature = repo.git(&["rev-parse", "feature"]);
    repo.git_fail(&["merge", "feature"]);
    let state = state(&repo);
    assert_eq!(
        state.in_progress,
        Some(InProgress::Merge),
        "merge in progress"
    );
    let merge = state.merge.as_ref().expect("merge details");
    assert_eq!(merge.heads.len(), 1, "one MERGE_HEAD");
    assert_eq!(merge.heads[0].to_string(), feature, "MERGE_HEAD");
    assert!(
        merge
            .message
            .as_deref()
            .is_some_and(|m| m.contains("Merge branch 'feature'")),
        "message: {:?}",
        merge.message
    );
    assert!(!merge.no_ff && !merge.autostash, "plain merge");
    assert_eq!(state.conflicted_paths, 1, "f.txt is conflicted");
    assert!(
        has_conflicts(&repo.open()).expect("has_conflicts"),
        "conflicts"
    );
    assert!(
        state.sequencer.is_none() && state.rebase.is_none(),
        "nothing else"
    );
}

#[test]
fn merge_resolved_needs_commit() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git_fail(&["merge", "feature"]);
    repo.write("f.txt", "resolved\n");
    repo.git(&["add", "f.txt"]);
    let state = state(&repo);
    assert_eq!(state.in_progress, Some(InProgress::Merge), "still merging");
    assert!(
        state.merge.is_some(),
        "merge details remain until the commit"
    );
    assert_eq!(state.conflicted_paths, 0, "resolved");
    assert!(
        !has_conflicts(&repo.open()).expect("has_conflicts"),
        "no conflicts"
    );
}

#[test]
fn merge_no_ff_mode_is_read() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git_fail(&["merge", "--no-ff", "feature"]);
    assert!(
        state(&repo).merge.expect("merge").no_ff,
        "MERGE_MODE says no-ff"
    );
}

#[test]
fn merge_autostash_flag() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.write("keep.txt", "locally modified\n");
    repo.git_fail(&["merge", "--autostash", "feature"]);
    let state = state(&repo);
    assert!(
        state.merge.expect("merge").autostash,
        "MERGE_AUTOSTASH exists"
    );
}

#[test]
fn cherry_pick_single() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let pick = repo.git(&["rev-parse", "feature~1"]);
    repo.git_fail(&["cherry-pick", "feature~1"]);
    let state = state(&repo);
    assert_eq!(
        state.in_progress,
        Some(InProgress::CherryPick),
        "cherry-pick"
    );
    let sequencer = state.sequencer.expect("sequencer details");
    assert_eq!(sequencer.kind, SequencerKind::CherryPick, "kind");
    assert_eq!(
        sequencer.current.map(|id| id.to_string()),
        Some(pick),
        "CHERRY_PICK_HEAD"
    );
    assert!(
        sequencer.remaining.is_empty(),
        "nothing remaining: {:?}",
        sequencer.remaining
    );
    assert_eq!(state.conflicted_paths, 1, "conflict");
}

#[test]
fn cherry_pick_sequence_remaining() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let first = repo.git(&["rev-parse", "feature~1"]);
    let second = repo.git(&["rev-parse", "feature"]);
    repo.git_fail(&["cherry-pick", "feature~1", "feature"]);
    let state = state(&repo);
    assert_eq!(
        state.in_progress,
        Some(InProgress::CherryPickSequence),
        "sequence"
    );
    let sequencer = state.sequencer.expect("sequencer details");
    assert_eq!(
        sequencer.current.map(|id| id.to_string()),
        Some(first),
        "current step"
    );
    assert_eq!(
        sequencer.remaining.len(),
        1,
        "one step remaining: {:?}",
        sequencer.remaining
    );
    assert_eq!(sequencer.remaining[0].action, TodoAction::Pick, "a pick");
    assert_eq!(
        sequencer.remaining[0].commit.map(|id| id.to_string()),
        Some(second),
        "abbreviation resolved"
    );
}

#[test]
fn revert() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    // Reverting the base's content change of f.txt conflicts with the later change on main.
    repo.write("f.txt", "again\n");
    repo.commit_all("main second");
    let target = repo.git(&["rev-parse", "HEAD~1"]);
    repo.git_fail(&["revert", "HEAD~1"]);
    let state = state(&repo);
    assert_eq!(state.in_progress, Some(InProgress::Revert), "revert");
    let sequencer = state.sequencer.expect("sequencer details");
    assert_eq!(sequencer.kind, SequencerKind::Revert, "kind");
    assert_eq!(
        sequencer.current.map(|id| id.to_string()),
        Some(target),
        "REVERT_HEAD"
    );
}

#[test]
fn rebase_merge_backend_conflict() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git(&["checkout", "-q", "feature"]);
    let onto = repo.git(&["rev-parse", "main"]);
    let orig = repo.git(&["rev-parse", "feature"]);
    repo.git_fail(&["rebase", "main"]);
    let state = state(&repo);
    // Git's merge backend writes `rebase-merge/interactive` even for a plain `git rebase` (git 2.56 does), and
    // `git status` and gix then call it interactive too: the marker file is the definition.
    assert!(
        matches!(
            state.in_progress,
            Some(InProgress::Rebase | InProgress::RebaseInteractive)
        ),
        "rebase: {:?}",
        state.in_progress
    );
    let rebase = state.rebase.expect("rebase details");
    assert_eq!(rebase.backend, RebaseBackend::Merge, "default backend");
    assert_eq!(
        rebase.head_name.as_ref().map(|n| n.as_bstr().to_string()),
        Some("refs/heads/feature".into()),
        "head-name"
    );
    assert_eq!(rebase.onto.map(|id| id.to_string()), Some(onto), "onto");
    assert_eq!(
        rebase.orig_head.map(|id| id.to_string()),
        Some(orig),
        "orig-head"
    );
    assert_eq!(rebase.step, Some((1, 2)), "first of two steps");
    assert_eq!(
        rebase.stop_reason,
        RebaseStopReason::Conflict,
        "conflict stop"
    );
    assert!(rebase.stopped_at.is_some(), "stopped-sha is resolved");
    assert!(!rebase.started_by_us, "started by plain git");
    assert_eq!(rebase.todo.len(), 1, "one step left: {:?}", rebase.todo);
    assert_eq!(state.conflicted_paths, 1, "conflict");
}

/// `git rebase -i main` with a sequence editor that is a `sed` script; Git exits 0 when it stops at `edit`/`break`.
fn rebase_i_with_editor(repo: &TestRepo, script: &str) {
    let editor = format!("sequence.editor=sed -i.bak '{script}'");
    repo.git(&["-c", editor.as_str(), "rebase", "-i", "main"]);
}

fn clean_diverged(env: &TestEnv) -> TestRepo {
    // Same shape as `diverged`, but the histories do not conflict, so only the todo drives the stops.
    let repo = env.repo("repo");
    repo.write("base.txt", "base\n");
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("a.txt", "a\n");
    repo.commit_all("feature a");
    repo.write("b.txt", "b\n");
    repo.commit_all("feature b");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("m.txt", "m\n");
    repo.commit_all("main m");
    repo.git(&["checkout", "-q", "feature"]);
    repo
}

#[test]
fn rebase_interactive_edit_stop_has_amend() {
    let env = TestEnv::new();
    let repo = clean_diverged(&env);
    let edited = repo.git(&["rev-parse", "feature~1"]);
    let last = repo.git(&["rev-parse", "feature"]);
    rebase_i_with_editor(&repo, "1s/^pick/edit/");
    let head = repo.git(&["rev-parse", "HEAD"]);
    let state = state(&repo);
    assert_eq!(
        state.in_progress,
        Some(InProgress::RebaseInteractive),
        "interactive rebase"
    );
    let rebase = state.rebase.expect("rebase details");
    assert!(rebase.interactive, "interactive");
    assert_eq!(rebase.stop_reason, RebaseStopReason::Edit, "edit stop");
    assert_eq!(
        rebase.amend.map(|id| id.to_string()),
        Some(head),
        "rebase-merge/amend names the commit HEAD may amend"
    );
    assert_eq!(
        rebase.stopped_at.map(|id| id.to_string()),
        Some(edited.clone()),
        "stopped-sha is the original commit"
    );
    assert_eq!(rebase.done.len(), 1, "one step done: {:?}", rebase.done);
    assert_eq!(rebase.done[0].action, TodoAction::Edit, "the edit line");
    assert_eq!(
        rebase.done[0].commit.map(|id| id.to_string()),
        Some(edited),
        "done line commit"
    );
    assert_eq!(rebase.todo.len(), 1, "one step left: {:?}", rebase.todo);
    assert_eq!(
        rebase.todo[0].commit.map(|id| id.to_string()),
        Some(last),
        "todo line commit"
    );
    assert_eq!(state.conflicted_paths, 0, "no conflict");
    assert!(!rebase.started_by_us, "no plan of ours");

    let plan_dir = repo.path.join(".git").join("gitcthulhu").join("rebase");
    std::fs::create_dir_all(&plan_dir).expect("plan dir");
    std::fs::write(plan_dir.join("plan.json"), "{}").expect("plan.json");
    assert!(
        repo_state(&repo.open())
            .expect("state")
            .rebase
            .expect("rebase")
            .started_by_us,
        "plan.json marks our own rebase"
    );
}

#[test]
fn rebase_break_stop() {
    let env = TestEnv::new();
    let repo = clean_diverged(&env);
    rebase_i_with_editor(&repo, "1s/$/\\nbreak/");
    let rebase = state(&repo).rebase.expect("rebase details");
    assert_eq!(
        rebase.stop_reason,
        RebaseStopReason::Break,
        "break stop: {rebase:?}"
    );
    assert_eq!(
        rebase.done.last().map(|l| l.action.clone()),
        Some(TodoAction::Break),
        "last done is break"
    );
}

#[test]
fn rebase_exec_failed_stop() {
    let env = TestEnv::new();
    let repo = clean_diverged(&env);
    repo.git_fail(&["rebase", "--exec", "exit 1", "main"]);
    let state = state(&repo);
    let rebase = state.rebase.expect("rebase details");
    assert_eq!(
        rebase.stop_reason,
        RebaseStopReason::ExecFailed,
        "exec stop: {rebase:?}"
    );
    assert_eq!(
        rebase.done.last().map(|l| l.action.clone()),
        Some(TodoAction::Exec),
        "last done is exec"
    );
}

#[test]
fn rebase_resolved_needs_continue() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git(&["checkout", "-q", "feature"]);
    repo.git_fail(&["rebase", "main"]);
    repo.write("f.txt", "resolved\n");
    repo.git(&["add", "f.txt"]);
    let rebase = state(&repo).rebase.expect("rebase details");
    assert_eq!(
        rebase.stop_reason,
        RebaseStopReason::ResolvedNeedsContinue,
        "resolved: {rebase:?}"
    );
}

#[test]
fn rebase_apply_backend() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git(&["checkout", "-q", "feature"]);
    repo.git_fail(&["rebase", "--apply", "main"]);
    let state = state(&repo);
    assert_eq!(state.in_progress, Some(InProgress::Rebase), "rebase");
    let rebase = state.rebase.expect("rebase details");
    assert_eq!(rebase.backend, RebaseBackend::Apply, "apply backend");
    assert_eq!(
        rebase.head_name.as_ref().map(|n| n.as_bstr().to_string()),
        Some("refs/heads/feature".into()),
        "head-name"
    );
    assert!(
        rebase.onto.is_some() && rebase.orig_head.is_some(),
        "onto and orig-head"
    );
    assert_eq!(rebase.step, Some((1, 2)), "next/last");
    assert_eq!(rebase.stop_reason, RebaseStopReason::Conflict, "conflict");
}

#[test]
fn am_in_progress() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let patch = repo.git(&["format-patch", "-1", "--stdout", "feature~1"]);
    repo.write("../change.patch", format!("{patch}\n"));
    repo.git_fail(&["am", "--3way", "../change.patch"]);
    let state = state(&repo);
    assert_eq!(state.in_progress, Some(InProgress::ApplyMailbox), "am");
    assert!(state.rebase.is_none(), "an am is not a rebase");
}

#[test]
fn bisect() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    repo.git(&["bisect", "start"]);
    repo.git(&["bisect", "bad", "feature"]);
    repo.git(&["bisect", "good", "main~1"]);
    let state = state(&repo);
    assert_eq!(state.in_progress, Some(InProgress::Bisect), "bisect");
    let bisect = state.bisect.expect("bisect details");
    assert_eq!(
        bisect.terms,
        ("bad".to_owned(), "good".to_owned()),
        "default terms"
    );
    assert_eq!(bisect.start.as_deref(), Some("main"), "started on main");
}

#[test]
fn linked_worktree_state_is_per_worktree() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    let linked = env.path("linked");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().expect("utf-8 path"),
        "-b",
        "wt",
        "feature",
    ]);
    repo.git_fail(&["merge", "feature"]); // conflicts in the main worktree only
    let main_state = state(&repo);
    assert_eq!(
        main_state.in_progress,
        Some(InProgress::Merge),
        "main worktree is merging"
    );
    assert!(!main_state.flags.linked_worktree, "main worktree");

    let linked_repo = Repo::open(&env.rt, &linked).expect("linked worktree opens");
    let linked_state = repo_state(&linked_repo).expect("state");
    assert_eq!(
        linked_state.in_progress, None,
        "the linked worktree is not merging"
    );
    assert!(linked_state.merge.is_none(), "no merge details");
    assert_eq!(linked_state.conflicted_paths, 0, "its own index is clean");
    assert!(linked_state.flags.linked_worktree, "flag set");
    match linked_state.head {
        HeadState::Branch { name, .. } => assert_eq!(name.as_bstr(), "refs/heads/wt", "own HEAD"),
        other => panic!("expected a branch, got {other:?}"),
    }
}

#[test]
fn bare_repository_state() {
    let env = TestEnv::new();
    let path = env.bare_remote("bare");
    let bare = Repo::open(&env.rt, &path).expect("bare repository opens");
    let state = repo_state(&bare).expect("state of a bare repository");
    assert!(state.flags.bare, "bare flag");
    assert!(!state.flags.has_submodules, "no work tree, no .gitmodules");
    assert_eq!(state.in_progress, None, "nothing in progress");
    assert_eq!(state.conflicted_paths, 0, "no index, no conflicts");
    match &state.head {
        HeadState::Unborn { name } => assert_eq!(name.as_bstr(), "refs/heads/main", "unborn main"),
        other => panic!("expected unborn, got {other:?}"),
    }
    assert!(
        !has_conflicts(&bare).expect("has_conflicts"),
        "a bare repository has no conflicts"
    );
}

#[test]
fn rebase_merge_dir_of_linked_worktree_is_not_seen_by_main() {
    let env = TestEnv::new();
    let repo = clean_diverged(&env);
    repo.git(&["checkout", "-q", "main"]);
    let linked = env.path("linked");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().expect("utf-8 path"),
        "feature",
    ]);
    let linked_fixture = TestRepo::init_with(&env, &linked, &["status"]);
    linked_fixture.git(&[
        "-c",
        "sequence.editor=sed -i.bak '1s/^pick/edit/'",
        "rebase",
        "-i",
        "main",
    ]);
    let linked_state =
        repo_state(&Repo::open(&env.rt, &linked).expect("linked opens")).expect("state");
    assert_eq!(
        linked_state.rebase.as_ref().map(|r| r.stop_reason),
        Some(RebaseStopReason::Edit),
        "the linked worktree is rebasing"
    );
    let main_state = state(&repo);
    assert!(
        main_state.rebase.is_none() && main_state.in_progress.is_none(),
        "the main worktree is not: {main_state:?}"
    );
}

#[test]
fn repo_flags() {
    let env = TestEnv::new();
    let repo = diverged(&env);
    assert!(!state(&repo).flags.has_submodules, "no submodules");
    repo.write(
        ".gitmodules",
        "[submodule \"x\"]\n\tpath = x\n\turl = ./x\n",
    );
    assert!(state(&repo).flags.has_submodules, ".gitmodules present");
    repo.git(&["config", "core.sparseCheckout", "true"]);
    let rt = env.rt.clone();
    let reopened = Repo::open(&rt, &repo.path).expect("open");
    assert!(
        repo_state(&reopened).expect("state").flags.sparse,
        "sparse checkout flag"
    );
}

#[test]
fn todo_line_parsing_all_commands() {
    let full = "0123456789abcdef0123456789abcdef01234567";

    let pick = parse_todo_line("p abc1234 Subject here").expect("pick");
    assert_eq!(pick.action, TodoAction::Pick, "p");
    assert_eq!(pick.abbrev.as_deref(), Some("abc1234"), "abbrev");
    assert_eq!(pick.rest, "Subject here", "subject");
    assert_eq!(
        pick.commit, None,
        "abbreviations are not resolved by the pure parser"
    );

    let long = parse_todo_line(&format!("pick {full} Subject")).expect("pick with full id");
    assert_eq!(
        long.commit.map(|id| id.to_string()).as_deref(),
        Some(full),
        "full ids are parsed"
    );
    // Recent Git writes the subject as a comment; the rest is kept as is.
    let commented = parse_todo_line(&format!("pick {full} # Subject")).expect("pick, new format");
    assert_eq!(
        commented.commit.map(|id| id.to_string()).as_deref(),
        Some(full),
        "the id before a commented subject"
    );
    assert_eq!(commented.rest, "# Subject", "rest kept verbatim");

    for (word, action) in [
        ("reword", TodoAction::Reword),
        ("r", TodoAction::Reword),
        ("edit", TodoAction::Edit),
        ("e", TodoAction::Edit),
        ("squash", TodoAction::Squash),
        ("s", TodoAction::Squash),
        ("drop", TodoAction::Drop),
        ("d", TodoAction::Drop),
    ] {
        let line = parse_todo_line(&format!("{word} abc1234 s")).expect("command");
        assert_eq!(line.action, action, "{word}");
        assert_eq!(line.abbrev.as_deref(), Some("abc1234"), "{word}: abbrev");
    }

    let fixup = parse_todo_line("fixup abc1234 s").expect("fixup");
    assert_eq!(
        fixup.action,
        TodoAction::Fixup(FixupFlag::None),
        "plain fixup"
    );
    let fixup_c = parse_todo_line("fixup -C abc1234 s").expect("fixup -C");
    assert_eq!(
        fixup_c.action,
        TodoAction::Fixup(FixupFlag::UseMessage),
        "fixup -C"
    );
    assert_eq!(
        fixup_c.abbrev.as_deref(),
        Some("abc1234"),
        "fixup -C: abbrev after the flag"
    );
    let fixup_lc = parse_todo_line("f -c abc1234 s").expect("fixup -c");
    assert_eq!(
        fixup_lc.action,
        TodoAction::Fixup(FixupFlag::EditMessage),
        "fixup -c"
    );

    let exec = parse_todo_line("exec make test -j4").expect("exec");
    assert_eq!(exec.action, TodoAction::Exec, "exec");
    assert_eq!(exec.rest, "make test -j4", "command");
    assert_eq!(
        parse_todo_line("x true").expect("x").action,
        TodoAction::Exec,
        "x"
    );
    assert_eq!(
        parse_todo_line("break").expect("break").action,
        TodoAction::Break,
        "break"
    );
    assert_eq!(
        parse_todo_line("b").expect("b").action,
        TodoAction::Break,
        "b"
    );
    assert_eq!(
        parse_todo_line("noop").expect("noop").action,
        TodoAction::Noop,
        "noop"
    );

    let label = parse_todo_line("label onto").expect("label");
    assert_eq!(label.action, TodoAction::Label, "label");
    assert_eq!(label.rest, "onto", "label name");
    assert_eq!(
        parse_todo_line("reset onto").expect("reset").action,
        TodoAction::Reset,
        "reset"
    );
    assert_eq!(
        parse_todo_line("t onto").expect("t").action,
        TodoAction::Reset,
        "t"
    );

    let merge = parse_todo_line("merge -C abc label # subject").expect("merge");
    assert_eq!(merge.action, TodoAction::Merge, "merge");
    assert_eq!(merge.abbrev.as_deref(), Some("abc"), "merge commit");
    assert_eq!(merge.rest, "label # subject", "label and subject");

    let update_ref = parse_todo_line("update-ref refs/heads/x").expect("update-ref");
    assert_eq!(update_ref.action, TodoAction::UpdateRef, "update-ref");
    assert_eq!(update_ref.rest, "refs/heads/x", "ref name");

    let comment = parse_todo_line("# Rebase abc..def onto 123").expect("comment");
    assert_eq!(comment.action, TodoAction::Comment, "comment");
    assert_eq!(comment.raw, "# Rebase abc..def onto 123", "raw text kept");

    assert!(parse_todo_line("").is_none(), "empty");
    assert!(parse_todo_line("   \t").is_none(), "blank");
    assert_eq!(
        parse_todo_line("frobnicate abc").expect("unknown").action,
        TodoAction::Unknown("frobnicate".into()),
        "unknown command"
    );
    assert_eq!(
        parse_todo_line("pick abc1234 s\r").expect("CRLF").rest,
        "s",
        "CRLF line ending is dropped"
    );
}
