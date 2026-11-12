//! Staging, unstaging and discarding (spec 20 §2.6 `stage.rs`, test matrix §4.8).
//!
//! Every partial case stages, unstages and discards a selection and compares the resulting index blob or worktree
//! bytes with a hand-written expectation. The base ids of each request come from Git itself (`git ls-files -s`,
//! `git ls-tree`, `git hash-object`), so they double as an oracle for the ids the diff would report.
//!
//! Tests of the whole-file functions spawn `git` through `cli::run_checked`, which WP-B0b implements; until it is
//! merged they are ignored with the reason "needs WP-B0b".

mod support;

use std::sync::Arc;

use bstr::ByteSlice as _;
use gitcthulhu_git::{
    CancelToken, DiscardMode, GitError, GitErrorKind, HunkHeader, LineRef, NoPrompter, OpContext,
    OpOutcome, PartialBlock, RefreshSet, RelaPath, Repo, Resolution, Selection, SelectionRequest,
    stage,
};
use support::{TestEnv, TestRepo};

// ───────────────────────────── helpers ─────────────────────────────

fn ctx(t: &TestRepo) -> OpContext {
    t.op_ctx(CancelToken::new(), Arc::new(NoPrompter))
}

fn oid(hex: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(hex.trim().as_bytes()).expect("git prints hex object ids")
}

/// The stage-0 index entry id of `path` (`git ls-files -s`).
fn index_id(t: &TestRepo, path: &str) -> Option<gix::ObjectId> {
    let out = t.git(&["ls-files", "-s", "--", &format!(":(literal){path}")]);
    out.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (_mode, id, stage) = (fields.next()?, fields.next()?, fields.next()?);
        (stage == "0").then(|| oid(id))
    })
}

/// The mode of the stage-0 index entry of `path`.
fn index_mode(t: &TestRepo, path: &str) -> Option<String> {
    let out = t.git(&["ls-files", "-s", "--", &format!(":(literal){path}")]);
    out.lines()
        .next()
        .and_then(|line| line.split_whitespace().next().map(str::to_owned))
}

/// HEAD's blob id at `path` (`git ls-tree`).
fn head_id(t: &TestRepo, path: &str) -> Option<gix::ObjectId> {
    let out = t.git(&["ls-tree", "HEAD", "--", path]);
    out.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(2).map(oid))
}

/// The id of the worktree file in git form (`git hash-object`, which applies the clean filters).
fn worktree_id(t: &TestRepo, path: &str) -> Option<gix::ObjectId> {
    t.path
        .join(path)
        .exists()
        .then(|| oid(&t.git(&["hash-object", "--", path])))
}

/// The paths in the index (`git ls-files`).
fn ls_files(t: &TestRepo) -> Vec<String> {
    t.git(&["ls-files"])
        .lines()
        .map(str::to_owned)
        .filter(|l| !l.is_empty())
        .collect()
}

fn lines(sel: &[LineRef]) -> Selection {
    Selection::Lines(sel.to_vec())
}

fn add(new_lineno: u32) -> LineRef {
    LineRef::Added { new_lineno }
}

fn rem(old_lineno: u32) -> LineRef {
    LineRef::Removed { old_lineno }
}

fn hunk(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> HunkHeader {
    HunkHeader {
        old_start,
        old_lines,
        new_start,
        new_lines,
    }
}

/// A request on the unstaged diff of `path` (index → worktree) with ids as Git sees them now.
fn unstaged_req(t: &TestRepo, path: &str, selection: Selection) -> SelectionRequest {
    SelectionRequest {
        path: path.into(),
        previous_path: None,
        base_old: index_id(t, path),
        base_new: worktree_id(t, path),
        selection,
    }
}

/// A request on the staged diff of `path` (HEAD → index) with ids as Git sees them now.
fn staged_req(
    t: &TestRepo,
    path: &str,
    previous_path: Option<&str>,
    selection: Selection,
) -> SelectionRequest {
    SelectionRequest {
        path: path.into(),
        previous_path: previous_path.map(Into::into),
        base_old: head_id(t, previous_path.unwrap_or(path)),
        base_new: index_id(t, path),
        selection,
    }
}

fn stage_sel(
    t: &TestRepo,
    repo: &Repo,
    req: &SelectionRequest,
) -> gitcthulhu_git::Result<OpOutcome<()>> {
    let mut guard = repo.lock_exclusive();
    stage::stage_selection(repo, guard.write_permission(), &ctx(t), req)
}

fn unstage_sel(
    t: &TestRepo,
    repo: &Repo,
    req: &SelectionRequest,
) -> gitcthulhu_git::Result<OpOutcome<()>> {
    let mut guard = repo.lock_exclusive();
    stage::unstage_selection(repo, guard.write_permission(), &ctx(t), req)
}

fn discard_sel(
    t: &TestRepo,
    repo: &Repo,
    req: &SelectionRequest,
) -> gitcthulhu_git::Result<OpOutcome<()>> {
    let mut guard = repo.lock_exclusive();
    stage::discard_selection(repo, guard.write_permission(), &ctx(t), req)
}

/// Stage `selection` of the unstaged diff and return the index blob of `path`.
fn stage_lines(t: &TestRepo, repo: &Repo, path: &str, selection: Selection) -> Vec<u8> {
    let outcome = stage_sel(t, repo, &unstaged_req(t, path, selection)).expect("staging works");
    assert_eq!(
        outcome.refresh,
        RefreshSet::STATUS,
        "staging only changes the status"
    );
    t.index_blob(path)
}

/// Unstage `selection` of the staged diff and return the index blob of `path`.
fn unstage_lines(t: &TestRepo, repo: &Repo, path: &str, selection: Selection) -> Vec<u8> {
    unstage_sel(t, repo, &staged_req(t, path, None, selection)).expect("unstaging works");
    t.index_blob(path)
}

/// Discard `selection` of the unstaged diff and return the worktree bytes of `path`.
fn discard_lines(t: &TestRepo, repo: &Repo, path: &str, selection: Selection) -> Vec<u8> {
    discard_sel(t, repo, &unstaged_req(t, path, selection)).expect("discarding works");
    t.read(path)
}

/// A repository with `f` committed as `old` and changed to `new` in the worktree.
fn modified(env: &TestEnv, old: &[u8], new: &[u8]) -> TestRepo {
    let t = env.repo("r");
    t.write("f", old);
    t.commit_all("base");
    t.write("f", new);
    t
}

fn numbered(changed: &[usize]) -> Vec<u8> {
    let mut out = String::new();
    for i in 1..=20 {
        if changed.contains(&i) {
            out.push_str(&format!("L{i:02}\n"));
        } else {
            out.push_str(&format!("l{i:02}\n"));
        }
    }
    out.into_bytes()
}

fn expect_partial_block(result: gitcthulhu_git::Result<OpOutcome<()>>, expected: PartialBlock) {
    match result {
        Err(GitError::PartialNotPossible { reason, .. }) => {
            assert_eq!(
                reason, expected,
                "the change can only be handled as a whole"
            )
        }
        other => panic!("expected PartialNotPossible({expected:?}), got {other:?}"),
    }
}

fn expect_kind(result: gitcthulhu_git::Result<OpOutcome<()>>, expected: GitErrorKind) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "unexpected error: {err:?}"),
        Ok(_) => panic!("expected {expected:?}, the operation succeeded"),
    }
}

// ───────────────────────────── whole hunks ─────────────────────────────

#[test]
fn hunk_first_middle_last() {
    let env = TestEnv::new();
    let t = modified(&env, &numbered(&[]), &numbered(&[1, 10, 20]));
    let repo = t.open();

    let staged = stage_lines(
        &t,
        &repo,
        "f",
        Selection::Hunks(vec![hunk(1, 4, 1, 4), hunk(17, 4, 17, 4)]),
    );
    assert_eq!(
        staged.as_bstr(),
        numbered(&[1, 20]).as_bstr(),
        "first and last hunk are staged, the middle one is not"
    );

    let unstaged = unstage_lines(&t, &repo, "f", Selection::Hunks(vec![hunk(17, 4, 17, 4)]));
    assert_eq!(
        unstaged.as_bstr(),
        numbered(&[1]).as_bstr(),
        "the last hunk of the staged diff is reverted in the index"
    );

    let discarded = discard_lines(&t, &repo, "f", Selection::Hunks(vec![hunk(7, 7, 7, 7)]));
    assert_eq!(
        discarded.as_bstr(),
        numbered(&[1, 20]).as_bstr(),
        "the middle hunk is reverted in the worktree, the rest stays"
    );
    assert_eq!(
        t.index_blob("f").as_bstr(),
        numbered(&[1]).as_bstr(),
        "discarding never touches the index"
    );
}

// ───────────────────────────── single lines ─────────────────────────────

#[test]
fn single_added_line() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb\nc\n", b"a\nb\nX\nY\nc\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(4)])).as_bstr(),
        b"a\nb\nY\nc\n".as_bstr(),
        "only Y is staged"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "Y is the third line of the staged side"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nY\nc\n".as_bstr(),
        "X is dropped from the worktree"
    );
}

#[test]
fn single_removed_line() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb\nc\nd\n", b"a\nd\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nc\nd\n".as_bstr(),
        "only the removal of b is staged"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nb\nc\nd\n".as_bstr(),
        "b is restored in the index"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[rem(3)])).as_bstr(),
        b"a\nc\nd\n".as_bstr(),
        "c comes back to the worktree, b stays removed"
    );
}

#[test]
fn replace_pair_select_only_add() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nold\nz\n", b"a\nnew\nz\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"a\nold\nnew\nz\n".as_bstr(),
        "the addition is staged, the removal is not"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nold\nz\n".as_bstr(),
        "the staged addition is reverted"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"a\nz\n".as_bstr(),
        "discarding only the addition keeps the removal of 'old'"
    );
}

#[test]
fn replace_pair_select_only_remove() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nold\nz\n", b"a\nnew\nz\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nz\n".as_bstr(),
        "the removal is staged, the addition is not"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nold\nz\n".as_bstr(),
        "the staged removal is reverted"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nold\nnew\nz\n".as_bstr(),
        "discarding only the removal brings 'old' back next to 'new'"
    );
}

// ───────────────────────────── end of file ─────────────────────────────

#[test]
fn no_eol_old_add_after() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb", b"a\nb\nc\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "the glue rule terminates the old last line before the staged one"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\n".as_bstr(),
        "unstaging c keeps the terminated b"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\n".as_bstr(),
        "c is dropped from the worktree"
    );
}

#[test]
fn no_eol_new_only() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb\n", b"a\nb\nc");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nc".as_bstr(),
        "the staged last line keeps its missing terminator"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\n".as_bstr(),
        "back to the committed content"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\n".as_bstr(),
        "the unterminated line is dropped"
    );
}

#[test]
fn no_eol_both() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb", b"a\nc");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\n".as_bstr(),
        "staging only the removal of b"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nb".as_bstr(),
        "b comes back without terminator"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", Selection::Hunks(vec![hunk(1, 2, 1, 2)])).as_bstr(),
        b"a\nb".as_bstr(),
        "discarding the whole hunk restores the unterminated b"
    );
}

// ───────────────────────────── line endings ─────────────────────────────

/// A repository whose git and gix sides both use `core.autocrlf=true` (the test runtime's gix overrides would
/// otherwise force `false`).
fn autocrlf_repo(env: &TestEnv) -> (TestRepo, Repo) {
    let t = env.repo("r");
    t.git(&["config", "core.autocrlf", "true"]);
    let rt = env
        .runtime_with(|config| {
            if let Some(isolation) = &mut config.test_isolation {
                isolation
                    .overrides
                    .retain(|(key, _)| key != "core.autocrlf");
                isolation
                    .overrides
                    .push(("core.autocrlf".into(), "true".into()));
            }
        })
        .expect("runtime with autocrlf");
    let repo = Repo::open(&rt, &t.path).expect("repository opens");
    (t, repo)
}

#[test]
fn crlf_autocrlf_true() {
    let env = TestEnv::new();
    let (t, repo) = autocrlf_repo(&env);
    t.write("f", b"a\r\nb\r\nc\r\n");
    t.commit_all("base");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "autocrlf stores LF"
    );
    t.write("f", b"a\r\nb\r\nX\r\nc\r\n");
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nX\nc\n".as_bstr(),
        "both images are compared in git form (LF)"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "back to the committed content"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\r\nb\r\nc\r\n".as_bstr(),
        "the discarded worktree file is converted back to CRLF"
    );
}

#[test]
fn crlf_committed_no_autocrlf() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\r\nb", b"a\r\nb\r\nc\r\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\r\nb\r\nc\r\n".as_bstr(),
        "the glue rule uses the file's dominant CRLF"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\r\nb\r\n".as_bstr(),
        "tokens keep their CRLF"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\r\nb\r\n".as_bstr(),
        "CRLF bytes are written back untouched"
    );
}

#[test]
fn mixed_eol_discard_refused_with_autocrlf() {
    let env = TestEnv::new();
    let (t, repo) = autocrlf_repo(&env);
    t.write("f", b"a\nb\nc\n");
    t.commit_all("base");
    t.write("f", b"a\r\nb\nX\r\nc\n");
    expect_partial_block(
        discard_sel(&t, &repo, &unstaged_req(&t, "f", lines(&[add(3)]))),
        PartialBlock::UnsafeRoundTrip,
    );
    assert_eq!(
        t.read("f").as_bstr(),
        b"a\r\nb\nX\r\nc\n".as_bstr(),
        "the refused discard left the file alone"
    );
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nX\nc\n".as_bstr(),
        "staging works on the git form regardless of the worktree mix"
    );
}

// ───────────────────────────── file kinds ─────────────────────────────

#[test]
fn untracked_partial() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("other", b"x\n");
    t.commit_all("base");
    t.write("f", b"a\nb\nc\n");
    let repo = t.open();

    let req = unstaged_req(&t, "f", lines(&[add(1), add(3)]));
    assert_eq!(req.base_old, None, "untracked files have no old side");
    stage_sel(&t, &repo, &req).expect("partial stage of an untracked file");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"a\nc\n".as_bstr(),
        "a new index entry with the selected lines"
    );
    assert_eq!(
        index_mode(&t, "f").as_deref(),
        Some("100644"),
        "mode from the worktree"
    );

    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"a\n".as_bstr(),
        "c is unstaged"
    );
    unstage_sel(&t, &repo, &staged_req(&t, "f", None, lines(&[add(1)])))
        .expect("unstaging the last line");
    assert_eq!(
        index_id(&t, "f"),
        None,
        "a file HEAD does not have leaves the index when every line is unstaged"
    );

    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"a\nc\n".as_bstr(),
        "discarding a line of an untracked file"
    );
}

/// `git add -N f` with `f` = "a\nb\n".
fn intent_to_add_repo(env: &TestEnv) -> TestRepo {
    let t = env.repo("r");
    t.write("other", b"x\n");
    t.commit_all("base");
    t.write("f", b"a\nb\n");
    t.git(&["add", "-N", "f"]);
    t
}

#[test]
fn intent_to_add_partial() {
    let env = TestEnv::new();
    let t = intent_to_add_repo(&env);
    let repo = t.open();
    let mut req = unstaged_req(&t, "f", lines(&[add(2)]));
    req.base_old = None; // the diff shows an intent-to-add entry like an untracked file
    stage_sel(&t, &repo, &req).expect("partial stage of an intent-to-add file");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"b\n".as_bstr(),
        "the selected line replaces the empty intent-to-add blob"
    );
    assert!(
        t.git(&["diff", "--cached", "--name-only"])
            .lines()
            .any(|l| l == "f"),
        "the entry is a real staged addition now (intent-to-add cleared)"
    );

    unstage_sel(&t, &repo, &staged_req(&t, "f", None, lines(&[add(1)])))
        .expect("unstaging everything");
    assert_eq!(index_id(&t, "f"), None, "the file is untracked again");

    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(1)])).as_bstr(),
        b"b\n".as_bstr(),
        "discarding a line of the (now untracked) file"
    );
}

#[test]
fn intent_to_add_partial_is_committed() {
    let env = TestEnv::new();
    let t = intent_to_add_repo(&env);
    let repo = t.open();
    let mut req = unstaged_req(&t, "f", lines(&[add(2)]));
    req.base_old = None;
    stage_sel(&t, &repo, &req).expect("partial stage of an intent-to-add file");
    t.git(&["commit", "-q", "-m", "partial"]);
    assert_eq!(
        t.git_bytes(&["show", "HEAD:f"]).as_bstr(),
        b"b\n".as_bstr(),
        "git commit takes the staged blob of the former intent-to-add entry"
    );
}

#[test]
fn deleted_all_lines_stages_deletion() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("f", b"a\nb\n");
    t.commit_all("base");
    t.remove("f");
    let repo = t.open();

    let req = unstaged_req(&t, "f", lines(&[rem(1), rem(2)]));
    assert_eq!(req.base_new, None, "a deleted file has no new side");
    stage_sel(&t, &repo, &req).expect("staging the deletion");
    assert_eq!(
        index_id(&t, "f"),
        None,
        "selecting every removed line stages the deletion"
    );

    let req = staged_req(&t, "f", None, lines(&[rem(1)]));
    assert_eq!(req.base_new, None, "a staged deletion has no new side");
    unstage_sel(&t, &repo, &req).expect("unstaging one removed line");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"a\n".as_bstr(),
        "the unstaged line comes back as a new index entry"
    );
    assert_eq!(
        index_mode(&t, "f").as_deref(),
        Some("100644"),
        "with HEAD's mode"
    );

    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[rem(1)])).as_bstr(),
        b"a\n".as_bstr(),
        "discarding the removal recreates the file"
    );
}

#[test]
fn deleted_some_lines() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("f", b"a\nb\nc\n");
    t.commit_all("base");
    t.remove("f");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nc\n".as_bstr(),
        "a partial deletion writes the remaining lines"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[rem(2)])).as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "back to HEAD"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[rem(1)])).as_bstr(),
        b"a\n".as_bstr(),
        "discarding one removed line recreates the file with it"
    );
}

#[cfg(unix)]
fn set_executable(t: &TestRepo, path: &str, executable: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(t.path.join(path), std::fs::Permissions::from_mode(mode))
        .expect("permissions can be set");
}

#[cfg(unix)]
fn is_executable(t: &TestRepo, path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(t.path.join(path))
        .expect("file exists")
        .permissions()
        .mode()
        & 0o111
        != 0
}

#[test]
#[cfg(unix)]
fn exec_bit_kept_on_partial() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\nb\n", b"a\nb\nc\nd\n");
    set_executable(&t, "f", true);
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(3)])).as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "content is staged partially"
    );
    assert_eq!(
        index_mode(&t, "f").as_deref(),
        Some("100644"),
        "the mode change is only staged by a whole-file stage"
    );
}

#[test]
fn staged_rename_partial_unstage() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("old.txt", b"a\nb\nc\n");
    t.commit_all("base");
    t.git(&["mv", "old.txt", "new.txt"]);
    t.write("new.txt", b"a\nb\nc\nd\n");
    t.git(&["add", "new.txt"]);
    let repo = t.open();

    let req = staged_req(&t, "new.txt", Some("old.txt"), lines(&[add(4)]));
    unstage_sel(&t, &repo, &req).expect("partial unstage of a staged rename");
    assert_eq!(
        t.index_blob("new.txt").as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "the result is written at the new path"
    );
    assert_eq!(
        ls_files(&t),
        ["new.txt"],
        "the previous path stays removed: a pure rename is left"
    );
}

#[test]
fn unstaged_rename_whole_file_only() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("old.txt", b"a\nb\n");
    t.commit_all("base");
    std::fs::rename(t.path.join("old.txt"), t.path.join("new.txt")).expect("rename");
    t.write("new.txt", b"a\nb\nc\n");
    let repo = t.open();
    let req = SelectionRequest {
        path: "new.txt".into(),
        previous_path: Some("old.txt".into()),
        base_old: head_id(&t, "old.txt"),
        base_new: worktree_id(&t, "new.txt"),
        selection: lines(&[add(3)]),
    };
    expect_partial_block(stage_sel(&t, &repo, &req), PartialBlock::Rename);
    expect_partial_block(discard_sel(&t, &repo, &req), PartialBlock::Rename);
}

#[test]
fn binary_whole_only() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\0b\n", b"a\0c\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(1)]));
    expect_partial_block(stage_sel(&t, &repo, &req), PartialBlock::Binary);
    expect_partial_block(discard_sel(&t, &repo, &req), PartialBlock::Binary);
}

#[test]
fn lfs_whole_only() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write(
        ".gitattributes",
        b"*.bin filter=lfs diff=lfs merge=lfs -text\n",
    );
    let pointer = |oid: &str| {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 3\n")
    };
    t.write("f.bin", pointer("aaaa"));
    t.commit_all("base");
    t.write("f.bin", pointer("bbbb"));
    let repo = t.open();
    let req = unstaged_req(&t, "f.bin", lines(&[add(2)]));
    expect_partial_block(stage_sel(&t, &repo, &req), PartialBlock::Lfs);
}

#[test]
fn latin1_bytes_preserved() {
    let env = TestEnv::new();
    let t = modified(&env, b"caf\xe9\nna\xefve\n", b"caf\xe9\nX\xe9\nna\xefve\n");
    let repo = t.open();
    assert_eq!(
        stage_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"caf\xe9\nX\xe9\nna\xefve\n".as_bstr(),
        "non-UTF-8 bytes are copied untouched"
    );
    assert_eq!(
        unstage_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"caf\xe9\nna\xefve\n".as_bstr(),
        "and removed untouched"
    );
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"caf\xe9\nna\xefve\n".as_bstr(),
        "the worktree keeps its Latin-1 bytes"
    );
}

#[test]
fn symlink_whole_only() {
    if !support::symlinks_supported() {
        eprintln!("skipped: symbolic links are not available");
        return;
    }
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("target", b"x\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink("target", t.path.join("link")).expect("symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file("target", t.path.join("link")).expect("symlink");
    let repo = t.open();
    let req = SelectionRequest {
        path: "link".into(),
        previous_path: None,
        base_old: None,
        base_new: None,
        selection: lines(&[add(1)]),
    };
    expect_partial_block(stage_sel(&t, &repo, &req), PartialBlock::Symlink);
}

// ───────────────────────────── races ─────────────────────────────

#[test]
fn stale_diff_detected_worktree_changed() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(2)]));
    t.write("f", b"a\nb\nc\n");
    expect_kind(stage_sel(&t, &repo, &req), GitErrorKind::StaleDiff);
    expect_kind(discard_sel(&t, &repo, &req), GitErrorKind::StaleDiff);
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"a\n".as_bstr(),
        "a stale request changes nothing"
    );
    assert_eq!(
        t.read("f").as_bstr(),
        b"a\nb\nc\n".as_bstr(),
        "the newer worktree content is kept"
    );
}

#[test]
fn stale_diff_detected_index_changed() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(2)]));
    // Someone stages other content meanwhile; the worktree ends up as it was.
    t.write("f", b"a\nb\nc\n");
    t.git(&["add", "f"]);
    t.write("f", b"a\nb\n");
    expect_kind(stage_sel(&t, &repo, &req), GitErrorKind::StaleDiff);
    expect_kind(discard_sel(&t, &repo, &req), GitErrorKind::StaleDiff);

    let staged = staged_req(&t, "f", None, lines(&[add(3)]));
    t.git(&["reset", "-q"]);
    expect_kind(unstage_sel(&t, &repo, &staged), GitErrorKind::StaleDiff);
}

#[test]
fn index_lock_exists() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(2)]));
    let lock = t.path.join(".git").join("index.lock");
    std::fs::write(&lock, b"").expect("lock file can be created");
    match stage_sel(&t, &repo, &req) {
        Err(GitError::LockExists { path }) => assert!(
            support::same_path(&path, &lock),
            "the error names the lock file, got {}",
            path.display()
        ),
        other => panic!("expected LockExists, got {other:?}"),
    }
    assert!(lock.exists(), "a foreign lock file is never removed by us");
    std::fs::remove_file(&lock).expect("lock file can be removed");
    stage_sel(&t, &repo, &req).expect("staging works once the lock is gone");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"a\nb\n".as_bstr(),
        "staged after the lock was released"
    );
}

#[test]
fn empty_selection_changes_nothing() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let index_before = std::fs::read(t.path.join(".git").join("index")).expect("index exists");
    // Line 1 is context, not a change: nothing is selected.
    stage_sel(&t, &repo, &unstaged_req(&t, "f", lines(&[add(1)]))).expect("no-op");
    assert_eq!(
        std::fs::read(t.path.join(".git").join("index")).expect("index exists"),
        index_before,
        "an empty selection does not write the index"
    );
}

#[test]
fn cancelled_context_does_nothing() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let cancel = CancelToken::new();
    cancel.cancel();
    let mut guard = repo.lock_exclusive();
    let result = stage::stage_selection(
        &repo,
        guard.write_permission(),
        &t.op_ctx(cancel, Arc::new(NoPrompter)),
        &unstaged_req(&t, "f", lines(&[add(2)])),
    );
    expect_kind(result, GitErrorKind::Cancelled);
}

// ───────────────────────────── paths (whole-file functions) ─────────────────────────────

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn stage_paths_glob_chars_literal() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("1.txt", b"1\n");
    t.write("2.txt", b"2\n");
    t.write("[12].txt", b"glob\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let outcome = stage::stage_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["[12].txt".into()],
    )
    .expect("git add works");
    assert_eq!(outcome.refresh, RefreshSet::STATUS, "status only");
    assert_eq!(ls_files(&t), ["[12].txt"], "the glob is taken literally");
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn discard_paths_glob_chars_literal() {
    let env = TestEnv::new();
    let t = env.repo("r");
    for name in ["1.txt", "2.txt", "[12].txt"] {
        t.write(name, b"base\n");
    }
    t.commit_all("base");
    for name in ["1.txt", "2.txt", "[12].txt"] {
        t.write(name, b"changed\n");
    }
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["[12].txt".into()],
        DiscardMode::Unstaged,
    )
    .expect("git restore works");
    assert_eq!(
        t.read("[12].txt").as_bstr(),
        b"base\n".as_bstr(),
        "the named file is restored"
    );
    assert_eq!(
        (t.read("1.txt"), t.read("2.txt")),
        (b"changed\n".to_vec(), b"changed\n".to_vec()),
        "files the glob would match are untouched"
    );
}

#[test]
#[cfg(unix)]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn colon_prefixed_name_literal() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write(":!x", b"magic\n");
    t.write("x", b"x\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::stage_paths(&repo, guard.write_permission(), &ctx(&t), &[":!x".into()])
        .expect("git add works");
    assert_eq!(
        ls_files(&t),
        [":!x"],
        "pathspec magic in a file name is literal"
    );
}

#[test]
#[cfg(unix)]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn star_in_name_literal() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("*.txt", b"star\n");
    t.write("a.txt", b"a\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::stage_paths(&repo, guard.write_permission(), &ctx(&t), &["*.txt".into()])
        .expect("git add works");
    assert_eq!(ls_files(&t), ["*.txt"], "the star is literal");
}

/// `f` conflicted: base "base", ours (main) "ours", theirs (side) "theirs".
fn conflicted_repo(env: &TestEnv) -> TestRepo {
    let t = env.repo("r");
    t.write("f", b"base\n");
    t.commit_all("base");
    t.git(&["checkout", "-q", "-b", "side"]);
    t.write("f", b"theirs\n");
    t.commit_all("theirs");
    t.git(&["checkout", "-q", "main"]);
    t.write("f", b"ours\n");
    t.commit_all("ours");
    t.git_fail(&["merge", "-q", "side"]);
    t
}

fn unmerged(t: &TestRepo) -> String {
    t.git(&["ls-files", "-u"])
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn discard_conflicted_path_unstaged_mode() {
    let env = TestEnv::new();
    let t = conflicted_repo(&env);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let outcome = stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["f".into()],
        DiscardMode::Unstaged,
    )
    .expect("an unmerged path is reset to HEAD");
    assert_eq!(
        outcome.refresh,
        RefreshSet::STATUS | RefreshSet::REPO_STATE,
        "discarding a conflict changes the repository state"
    );
    assert_eq!(unmerged(&t), "", "no conflict stages are left");
    assert_eq!(
        t.read("f").as_bstr(),
        b"ours\n".as_bstr(),
        "HEAD's version is kept"
    );
}

// ───────────────────────────── other whole-file behaviour ─────────────────────────────

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn stage_all_and_unstage_all() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a", b"a\n");
    t.commit_all("base");
    t.write("a", b"a2\n");
    t.write("b", b"b\n");
    let repo = t.open();
    {
        let mut guard = repo.lock_exclusive();
        stage::stage_all(&repo, guard.write_permission(), &ctx(&t)).expect("git add -A");
    }
    assert_eq!(
        t.git(&["diff", "--cached", "--name-only"]),
        "a\nb",
        "everything is staged"
    );
    {
        let mut guard = repo.lock_exclusive();
        stage::unstage_all(&repo, guard.write_permission(), &ctx(&t)).expect("git reset");
    }
    assert_eq!(
        t.git(&["diff", "--cached", "--name-only"]),
        "",
        "nothing is staged"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn unstage_paths_born_and_unborn() {
    let env = TestEnv::new();
    let unborn = env.repo("unborn");
    unborn.write("a", b"a\n");
    unborn.write("b", b"b\n");
    unborn.git(&["add", "-A"]);
    let repo = unborn.open();
    {
        let mut guard = repo.lock_exclusive();
        stage::unstage_paths(
            &repo,
            guard.write_permission(),
            &ctx(&unborn),
            &["a".into()],
        )
        .expect("git rm --cached on an unborn HEAD");
    }
    assert_eq!(ls_files(&unborn), ["b"], "only a was unstaged");
    {
        let mut guard = repo.lock_exclusive();
        stage::unstage_all(&repo, guard.write_permission(), &ctx(&unborn))
            .expect("git rm --cached -r . on an unborn HEAD");
    }
    assert!(ls_files(&unborn).is_empty(), "nothing is left in the index");

    let born = env.repo("born");
    born.write("a", b"a\n");
    born.commit_all("base");
    born.write("a", b"a2\n");
    born.git(&["add", "a"]);
    let repo = born.open();
    let mut guard = repo.lock_exclusive();
    stage::unstage_paths(&repo, guard.write_permission(), &ctx(&born), &["a".into()])
        .expect("git restore --staged");
    assert_eq!(
        born.index_blob("a").as_bstr(),
        b"a\n".as_bstr(),
        "the index is back at HEAD"
    );
    assert_eq!(
        born.read("a").as_bstr(),
        b"a2\n".as_bstr(),
        "the worktree is untouched"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn unstage_all_keeps_the_merge_in_progress() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a", b"a\n");
    t.commit_all("base");
    t.git(&["checkout", "-q", "-b", "side"]);
    t.write("b", b"b\n");
    t.commit_all("side");
    t.git(&["checkout", "-q", "main"]);
    t.write("c", b"c\n");
    t.commit_all("main");
    t.git(&["merge", "-q", "--no-ff", "--no-commit", "side"]);
    assert_eq!(
        t.git(&["diff", "--cached", "--name-only"]),
        "b",
        "precondition: the merge result is staged"
    );
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let outcome =
        stage::unstage_all(&repo, guard.write_permission(), &ctx(&t)).expect("unstage all");
    assert_eq!(outcome.refresh, RefreshSet::STATUS, "status only");
    assert_eq!(
        t.git(&["diff", "--cached", "--name-only"]),
        "",
        "nothing is staged"
    );
    assert!(
        t.path.join(".git").join("MERGE_HEAD").exists(),
        "unstaging does not abort the merge (a pathless `git reset` would delete MERGE_HEAD)"
    );
}

#[test]
fn unstage_all_refuses_conflicts() {
    let env = TestEnv::new();
    let t = conflicted_repo(&env);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    expect_partial_block(
        stage::unstage_all(&repo, guard.write_permission(), &ctx(&t)),
        PartialBlock::Conflict,
    );
    assert_ne!(unmerged(&t), "", "the conflict is untouched");
    assert!(
        t.path.join(".git").join("MERGE_HEAD").exists(),
        "the merge is still in progress"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn discard_paths_staged_and_unstaged_removes_added_file() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a", b"a\n");
    t.commit_all("base");
    t.write("a", b"a2\n");
    t.write("new", b"new\n");
    t.git(&["add", "-A"]);
    t.write("a", b"a3\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["a".into(), "new".into()],
        DiscardMode::StagedAndUnstaged,
    )
    .expect("git restore --source=HEAD --staged --worktree");
    assert_eq!(t.read("a").as_bstr(), b"a\n".as_bstr(), "a is back at HEAD");
    assert!(
        !t.path.join("new").exists(),
        "the staged new file is removed from the worktree"
    );
    assert_eq!(ls_files(&t), ["a"], "and from the index");
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn discard_paths_unborn_staged_and_unstaged() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("a", b"a\n");
    t.git(&["add", "a"]);
    t.write("untracked", b"u\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["a".into()],
        DiscardMode::StagedAndUnstaged,
    )
    .expect("git rm --cached plus file deletion");
    assert!(ls_files(&t).is_empty(), "the index is empty again");
    assert!(!t.path.join("a").exists(), "the file is gone");
    assert!(
        t.path.join("untracked").exists(),
        "unrelated untracked files stay"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn resolve_conflict_ours_theirs_mark_and_delete() {
    for (how, expected) in [
        (Resolution::Ours, Some(&b"ours\n"[..])),
        (Resolution::Theirs, Some(&b"theirs\n"[..])),
        (Resolution::Delete, None),
    ] {
        let env = TestEnv::new();
        let t = conflicted_repo(&env);
        let repo = t.open();
        let mut guard = repo.lock_exclusive();
        let outcome = stage::resolve_conflict(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            &["f".into()],
            how,
        )
        .expect("the conflict can be resolved");
        assert_eq!(
            outcome.refresh,
            RefreshSet::STATUS | RefreshSet::REPO_STATE,
            "resolving changes status and state"
        );
        assert_eq!(unmerged(&t), "", "{how:?}: no conflict stages are left");
        match expected {
            Some(content) => {
                assert_eq!(
                    t.index_blob("f").as_bstr(),
                    content.as_bstr(),
                    "{how:?}: the chosen side is staged"
                );
            }
            None => assert_eq!(index_id(&t, "f"), None, "{how:?}: the path is removed"),
        }
    }

    let env = TestEnv::new();
    let t = conflicted_repo(&env);
    t.write("f", b"merged by hand\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::resolve_conflict(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["f".into()],
        Resolution::MarkResolved,
    )
    .expect("mark resolved");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"merged by hand\n".as_bstr(),
        "the worktree file is staged as the resolution"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn resolve_conflict_ours_keeps_an_already_resolved_path() {
    let env = TestEnv::new();
    let t = env.repo("r");
    for name in ["f", "g"] {
        t.write(name, b"base\n");
    }
    t.commit_all("base");
    t.git(&["checkout", "-q", "-b", "side"]);
    for name in ["f", "g"] {
        t.write(name, b"theirs\n");
    }
    t.commit_all("theirs");
    t.git(&["checkout", "-q", "main"]);
    for name in ["f", "g"] {
        t.write(name, b"ours\n");
    }
    t.commit_all("ours");
    t.git_fail(&["merge", "-q", "side"]);
    // The user resolves g by hand and stages it, then keeps editing it.
    t.write("g", b"merged by hand\n");
    t.git(&["add", "g"]);
    t.write("g", b"merged by hand, edited\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::resolve_conflict(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["f".into(), "g".into()],
        Resolution::Ours,
    )
    .expect("both paths can be resolved");
    assert_eq!(unmerged(&t), "", "no conflict stages are left");
    assert_eq!(
        t.index_blob("f").as_bstr(),
        b"ours\n".as_bstr(),
        "f takes our side"
    );
    assert_eq!(
        t.read("g").as_bstr(),
        b"merged by hand, edited\n".as_bstr(),
        "the resolved path is not checked out from the index (its edits survive)"
    );
    assert_eq!(
        t.index_blob("g").as_bstr(),
        b"merged by hand, edited\n".as_bstr(),
        "the resolved path is staged as it is"
    );
}

#[test]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn resolve_conflict_ours_on_deleted_by_us_removes() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("f", b"base\n");
    t.commit_all("base");
    t.git(&["checkout", "-q", "-b", "side"]);
    t.write("f", b"theirs\n");
    t.commit_all("theirs");
    t.git(&["checkout", "-q", "main"]);
    t.git(&["rm", "-q", "f"]);
    t.git(&["commit", "-q", "-m", "delete"]);
    t.git_fail(&["merge", "-q", "side"]);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    stage::resolve_conflict(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["f".into()],
        Resolution::Ours,
    )
    .expect("taking our deletion");
    assert_eq!(unmerged(&t), "", "no conflict stages are left");
    assert_eq!(index_id(&t, "f"), None, "our side deleted the file");
}

#[test]
#[cfg(unix)]
#[ignore = "needs WP-B0b (cli::run_checked)"]
fn whole_content_with_exec_bit_change_stages_whole_file() {
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    set_executable(&t, "f", true);
    let repo = t.open();
    stage_sel(&t, &repo, &unstaged_req(&t, "f", lines(&[add(2)])))
        .expect("all lines plus the mode change");
    assert_eq!(
        index_mode(&t, "f").as_deref(),
        Some("100755"),
        "selecting everything stages the mode change too, through git add"
    );
}

// ───────────────────────────── whole-file checks that run no git ─────────────────────────────

#[test]
fn whole_file_functions_refuse_paths_outside_the_worktree() {
    let env = TestEnv::new();
    let t = env.repo("r");
    let repo = t.open();
    for bad in ["../outside", "/abs", ".git/config", "a/../../b"] {
        let mut guard = repo.lock_exclusive();
        let result = stage::stage_paths(&repo, guard.write_permission(), &ctx(&t), &[bad.into()]);
        expect_kind(result, GitErrorKind::InvalidName);
        let result =
            stage::delete_untracked(&repo, guard.write_permission(), &ctx(&t), &[bad.into()]);
        expect_kind(result, GitErrorKind::InvalidName);
    }
}

#[test]
fn unstage_paths_refuses_conflicted() {
    let env = TestEnv::new();
    let t = conflicted_repo(&env);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    expect_partial_block(
        stage::unstage_paths(&repo, guard.write_permission(), &ctx(&t), &["f".into()]),
        PartialBlock::Conflict,
    );
    assert_ne!(unmerged(&t), "", "the conflict is untouched");
}

#[test]
fn conflicted_path_refuses_partial_operations() {
    let env = TestEnv::new();
    let t = conflicted_repo(&env);
    let repo = t.open();
    let req = SelectionRequest {
        path: "f".into(),
        previous_path: None,
        base_old: None,
        base_new: worktree_id(&t, "f"),
        selection: lines(&[add(1)]),
    };
    expect_partial_block(stage_sel(&t, &repo, &req), PartialBlock::Conflict);
    expect_partial_block(unstage_sel(&t, &repo, &req), PartialBlock::Conflict);
    expect_partial_block(discard_sel(&t, &repo, &req), PartialBlock::Conflict);
}

#[test]
fn delete_untracked_removes_files_and_directories() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("tracked", b"t\n");
    t.commit_all("base");
    t.write("junk.txt", b"j\n");
    t.write("build/out/a.o", b"o\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let outcome = stage::delete_untracked(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["junk.txt".into(), "build/".into(), "already-gone".into()],
    )
    .expect("untracked paths are deleted");
    assert_eq!(outcome.refresh, RefreshSet::STATUS, "status only");
    assert!(!t.path.join("junk.txt").exists(), "the file is deleted");
    assert!(
        !t.path.join("build").exists(),
        "the directory is deleted recursively"
    );
    assert!(t.path.join("tracked").exists(), "tracked files stay");
}

#[test]
fn delete_untracked_refuses_tracked_paths_before_deleting_anything() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("dir/tracked", b"t\n");
    t.commit_all("base");
    t.write("junk.txt", b"j\n");
    let repo = t.open();
    for tracked in ["dir/tracked", "dir"] {
        let mut guard = repo.lock_exclusive();
        let result = stage::delete_untracked(
            &repo,
            guard.write_permission(),
            &ctx(&t),
            &["junk.txt".into(), RelaPath::from(tracked)],
        );
        match result {
            Err(GitError::InvalidName { what, name }) => {
                assert_eq!(what, "untracked path", "the path is tracked");
                assert_eq!(name, tracked, "the error names the tracked path");
            }
            other => panic!("expected InvalidName for {tracked}, got {other:?}"),
        }
        assert!(
            t.path.join("junk.txt").exists(),
            "nothing is deleted when one path is refused"
        );
    }
}

#[test]
fn delete_untracked_refuses_case_variant_of_tracked_path() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("README.md", b"tracked\n");
    t.commit_all("base");
    t.git(&["config", "core.ignoreCase", "true"]);
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let result = stage::delete_untracked(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["readme.MD".into()],
    );
    expect_kind(result, GitErrorKind::InvalidName);
    // In a discard the case variant counts as tracked: Git either restores the file from the index or refuses the
    // pathspec. Either outcome is fine; deleting it as an untracked file is not.
    let _ = stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["readme.MD".into()],
        DiscardMode::Unstaged,
    );
    assert_eq!(
        t.read("README.md").as_bstr(),
        b"tracked\n".as_bstr(),
        "with core.ignoreCase the case variant names the tracked file, which stays"
    );
}

/// A directory link at `link` pointing to `target`: a symlink, or on Windows without symlink rights a junction
/// (which needs no privileges and is just as able to lead outside the worktree).
fn dir_link(target: &std::path::Path, link: &std::path::Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("symlink");
    #[cfg(windows)]
    if support::symlinks_supported() {
        std::os::windows::fs::symlink_dir(target, link).expect("symlink");
    } else {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("cmd runs");
        assert!(status.success(), "mklink /J creates a junction");
    }
}

#[test]
fn delete_untracked_never_follows_a_symlinked_directory() {
    let env = TestEnv::new();
    let outside = env.path("outside");
    std::fs::create_dir_all(&outside).expect("outside directory");
    std::fs::write(outside.join("victim"), b"precious\n").expect("outside file");
    let t = env.repo("r");
    t.write("tracked", b"t\n");
    t.commit_all("base");
    dir_link(&outside, &t.path.join("link"));
    let repo = t.open();

    let mut guard = repo.lock_exclusive();
    let result = stage::delete_untracked(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["link/victim".into()],
    );
    expect_kind(result, GitErrorKind::InvalidName);
    assert!(
        outside.join("victim").exists(),
        "nothing outside the worktree is deleted through a link"
    );

    stage::delete_untracked(&repo, guard.write_permission(), &ctx(&t), &["link".into()])
        .expect("the untracked link itself can be deleted");
    assert!(
        std::fs::symlink_metadata(t.path.join("link")).is_err(),
        "the link is gone"
    );
    assert!(
        outside.join("victim").exists(),
        "deleting the link leaves its target alone"
    );
}

#[test]
fn discard_paths_deletes_untracked_without_git() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("tracked", b"t\n");
    t.commit_all("base");
    t.write("junk.txt", b"j\n");
    let repo = t.open();
    let mut guard = repo.lock_exclusive();
    let outcome = stage::discard_paths(
        &repo,
        guard.write_permission(),
        &ctx(&t),
        &["junk.txt".into()],
        DiscardMode::Unstaged,
    )
    .expect("untracked paths in a discard are deleted");
    assert!(
        outcome.log.commands.is_empty(),
        "no git command is needed for untracked paths"
    );
    assert!(!t.path.join("junk.txt").exists(), "the file is deleted");
}

// ───────────────────────────── other ─────────────────────────────

#[test]
#[cfg(unix)]
fn discard_preserves_file_mode() {
    let env = TestEnv::new();
    let t = env.repo("r");
    t.write("f", b"a\n");
    set_executable(&t, "f", true);
    t.commit_all("base");
    t.write("f", b"a\nb\nc\n");
    set_executable(&t, "f", true);
    let repo = t.open();
    assert_eq!(
        discard_lines(&t, &repo, "f", lines(&[add(2)])).as_bstr(),
        b"a\nc\n".as_bstr(),
        "one line is discarded"
    );
    assert!(
        is_executable(&t, "f"),
        "the atomic rewrite keeps the executable bit"
    );
}

#[test]
#[cfg(windows)]
fn discard_open_file_on_windows_reports_io() {
    use std::os::windows::fs::OpenOptionsExt as _;
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(2)]));
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(t.path.join("f"))
        .expect("the file can be opened exclusively");
    let result = discard_sel(&t, &repo, &req);
    drop(held);
    match result {
        Err(GitError::Io { path, .. }) => assert!(
            path.ends_with("f"),
            "the error names the file, got {}",
            path.display()
        ),
        other => panic!("expected an Io error, got {other:?}"),
    }
    let leftovers: Vec<_> = std::fs::read_dir(&t.path)
        .expect("worktree can be listed")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp-gitcthulhu-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temporary file is left behind: {leftovers:?}"
    );
    assert_eq!(
        t.read("f").as_bstr(),
        b"a\nb\n".as_bstr(),
        "the file is unchanged"
    );
}

/// Like `discard_open_file_on_windows_reports_io`, but the other program allows reading: the content can be read and
/// the temporary file written, so it is the final rename over the open file that fails.
#[test]
#[cfg(windows)]
fn discard_rename_over_open_file_on_windows_reports_io_and_cleans_up() {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_READ: u32 = 0x1;
    let env = TestEnv::new();
    let t = modified(&env, b"a\n", b"a\nb\n");
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(2)]));
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(t.path.join("f"))
        .expect("the file can be opened");
    let result = discard_sel(&t, &repo, &req);
    drop(held);
    match result {
        Err(GitError::Io { path, .. }) => assert!(
            path.ends_with("f"),
            "the error names the target file, got {}",
            path.display()
        ),
        other => panic!("expected an Io error from the rename, got {other:?}"),
    }
    let leftovers: Vec<_> = std::fs::read_dir(&t.path)
        .expect("worktree can be listed")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp-gitcthulhu-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the temporary file is removed after the failed rename: {leftovers:?}"
    );
    assert_eq!(
        t.read("f").as_bstr(),
        b"a\nb\n".as_bstr(),
        "the file is unchanged"
    );
}

#[test]
#[ignore = "performance: run in release; asserts only with GITCTHULHU_PERF_ASSERT=1"]
fn big_file_50k_lines_under_50ms() {
    let env = TestEnv::new();
    let old: String = (0..50_000).map(|i| format!("line {i}\n")).collect();
    let new: String = (0..50_000)
        .map(|i| {
            if i % 100 == 0 {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    let t = modified(&env, old.as_bytes(), new.as_bytes());
    let repo = t.open();
    let req = unstaged_req(&t, "f", lines(&[add(1), rem(1), add(25_001), rem(25_001)]));
    let start = std::time::Instant::now();
    stage_sel(&t, &repo, &req).expect("staging works");
    let elapsed = start.elapsed();
    eprintln!("stage_selection on a 50k-line file: {elapsed:?}");
    if std::env::var_os("GITCTHULHU_PERF_ASSERT").is_some_and(|v| v == "1") {
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "budget of §9: {elapsed:?}"
        );
    }
}
