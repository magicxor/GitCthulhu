//! B: the restore matrix. Every operation is recorded, then undone, redone and undone again; after each step the
//! repository fingerprint must equal the state before resp. after the operation, and `git fsck` must pass. The
//! sequences "undo" and "undo, redo" of the spec are the prefixes of "undo, redo, undo", so one run per operation
//! and worktree condition checks all three.

use std::path::Path;

use gitcthulhu_oplog::{OperationDescriptor, OperationKind, UntrackedCapture};

use crate::{
    assert_no_journal, done, fp, fsck, redo,
    support::{Fixture, b, desc, git_in, record_ok, refs_only},
    undo,
};

/// Worktree conditions created before the operation and not touched by it.
#[derive(Clone, Copy, Debug)]
enum Condition {
    /// (i)
    Clean,
    /// (ii) an unrelated modified tracked file.
    ModifiedTracked,
    /// (iii) an unrelated untracked file.
    Untracked,
    /// (iv) an unrelated staged change.
    Staged,
}

impl Condition {
    fn apply(self, fx: &Fixture) {
        match self {
            Condition::Clean => {}
            Condition::ModifiedTracked => fx.write("unrelated.txt", "unrelated, modified\n"),
            Condition::Untracked => fx.write("untracked-unrelated.txt", "untracked\n"),
            Condition::Staged => {
                fx.write("staged.txt", "staged, changed\n");
                fx.git(&["add", "staged.txt"]);
            }
        }
    }
}

/// A step run against the fixture.
type Step = Box<dyn FnOnce(&Fixture)>;

/// The operation under test: its descriptor, what it does, and what happens after it was recorded.
struct Op {
    desc: OperationDescriptor,
    run: Step,
    after: Option<Step>,
}

impl Op {
    fn new(desc: OperationDescriptor, run: impl FnOnce(&Fixture) + 'static) -> Op {
        Op {
            desc,
            run: Box::new(run),
            after: None,
        }
    }

    fn full(kind: OperationKind, title: &str, run: impl FnOnce(&Fixture) + 'static) -> Op {
        Op::new(desc(kind, title), run)
    }

    fn refs_only(kind: OperationKind, title: &str, run: impl FnOnce(&Fixture) + 'static) -> Op {
        Op::new(refs_only(kind, title), run)
    }

    fn untracked(mut self, untracked: UntrackedCapture) -> Op {
        self.desc.untracked = untracked;
        self
    }

    fn then(mut self, after: impl FnOnce(&Fixture) + 'static) -> Op {
        self.after = Some(Box::new(after));
        self
    }
}

/// `a.txt`, `unrelated.txt` and `staged.txt`, each committed separately.
fn base_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.commit_file("unrelated.txt", "unrelated\n", "add unrelated");
    fx.commit_file("staged.txt", "staged\n", "add staged");
    fx
}

/// Set up `fx` for an operation and return it; then apply `condition`, record, and check undo/redo/undo.
fn check(fx: Fixture, condition: Condition, make: impl FnOnce(&Fixture) -> Op) {
    let op = make(&fx);
    condition.apply(&fx);
    let oplog = fx.oplog();
    let before = fp(&fx);
    record_ok(&fx, &oplog, op.desc, op.run);
    if let Some(after) = op.after {
        after(&fx);
    }
    let after = fp(&fx);
    assert_ne!(before, after, "the operation must change the repository");

    done(undo(&fx, &oplog));
    assert_eq!(
        fp(&fx),
        before,
        "undo restores the state before the operation"
    );
    fsck(&fx);
    assert_no_journal(&fx);

    done(redo(&fx, &oplog));
    assert_eq!(
        fp(&fx),
        after,
        "redo restores the state after the operation"
    );
    fsck(&fx);

    done(undo(&fx, &oplog));
    assert_eq!(
        fp(&fx),
        before,
        "undo after redo restores the state before again"
    );
    fsck(&fx);
    assert_no_journal(&fx);
}

/// A bare repository next to `fx` as `origin`, with `main` pushed.
fn add_remote(fx: &Fixture) -> std::path::PathBuf {
    let remote = fx.tmp().join("remote.git");
    std::fs::create_dir_all(&remote).expect("the remote directory can be created");
    git_in(&remote, &["init", "-q", "--bare", "-b", "main"]);
    fx.git(&["remote", "add", "origin", &slash(&remote)]);
    fx.git(&["push", "-q", "origin", "main"]);
    remote
}

/// A clone of `remote` with an identity, for commits made "elsewhere".
fn other_clone(fx: &Fixture, remote: &Path) -> Fixture {
    let path = fx.tmp().join("other");
    git_in(fx.tmp(), &["clone", "-q", &slash(remote), &slash(&path)]);
    for (key, value) in [
        ("user.name", "Other User"),
        ("user.email", "other@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        git_in(&path, &["config", key, value]);
    }
    fx.worktree(path)
}

/// A path with forward slashes, which Git and its shell accept on every platform.
fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The untracked files of the newest stash.
fn stash_untracked_paths(fx: &Fixture) -> UntrackedCapture {
    let names = fx.git(&["ls-tree", "-r", "--name-only", "stash@{0}^3"]);
    UntrackedCapture::Paths(names.lines().map(b).collect())
}

/// `feature` (from the base commit) adds `b.txt`; `main` stays checked out.
fn feature_branch(fx: &Fixture) {
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("b.txt", "b\n", "add b");
    fx.git(&["checkout", "-q", "main"]);
}

// ── the operations ───────────────────────────────────────────────────────────────────────────

fn commit(_: &Fixture) -> Op {
    Op::full(OperationKind::Commit, "Commit \"change a\"", |fx| {
        fx.commit_file("a.txt", "a, changed\n", "change a");
    })
}

fn commit_new_and_staged(fx: &Fixture) -> Op {
    fx.write("new.txt", "new\n");
    fx.git(&["add", "new.txt"]);
    fx.write("a.txt", "a, staged\n");
    fx.git(&["add", "a.txt"]);
    fx.write("loose.txt", "stays untracked\n");
    Op::full(OperationKind::Commit, "Commit \"new and staged\"", |fx| {
        fx.git(&["commit", "-q", "-m", "new and staged"]);
    })
}

fn amend(_: &Fixture) -> Op {
    Op::full(OperationKind::Amend, "Amend \"add staged\"", |fx| {
        fx.write("a.txt", "a, amended\n");
        fx.git(&["add", "a.txt"]);
        fx.git(&["commit", "-q", "--amend", "-m", "amended"]);
    })
}

fn checkout_branch_carrying_changes(fx: &Fixture) -> Op {
    feature_branch(fx);
    fx.write("a.txt", "a, local change\n");
    Op::full(OperationKind::Checkout, "Check out 'feature'", |fx| {
        fx.git(&["checkout", "-q", "feature"]);
    })
}

fn checkout_detached(fx: &Fixture) -> Op {
    fx.commit_file("b.txt", "b\n", "add b");
    Op::full(OperationKind::CheckoutCommit, "Check out HEAD~1", |fx| {
        fx.git(&["checkout", "-q", "--detach", "HEAD~1"]);
    })
}

fn checkout_remote_branch(fx: &Fixture) -> Op {
    add_remote(fx);
    fx.git(&["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    fx.git(&["fetch", "-q", "origin"]);
    Op::full(
        OperationKind::CheckoutRemoteBranch,
        "Check out 'origin/feature'",
        |fx| {
            fx.git(&[
                "checkout",
                "-q",
                "-b",
                "feature",
                "--track",
                "origin/feature",
            ]);
        },
    )
}

fn create_branch(_: &Fixture) -> Op {
    Op::refs_only(OperationKind::CreateBranch, "Create branch 'topic'", |fx| {
        fx.git(&["branch", "topic"]);
    })
}

fn create_and_checkout_branch(_: &Fixture) -> Op {
    Op::full(
        OperationKind::CreateBranchAndCheckout,
        "Create and check out 'topic'",
        |fx| {
            fx.git(&["checkout", "-q", "-b", "topic"]);
        },
    )
}

fn delete_branch(fx: &Fixture) -> Op {
    fx.git(&["branch", "topic", "HEAD~1"]);
    fx.git(&["config", "branch.topic.remote", "origin"]);
    fx.git(&["config", "branch.topic.merge", "refs/heads/topic"]);
    Op::refs_only(OperationKind::DeleteBranch, "Delete branch 'topic'", |fx| {
        fx.git(&["branch", "-q", "-D", "topic"]);
    })
}

fn rename_current_branch(fx: &Fixture) -> Op {
    fx.git(&["config", "branch.main.remote", "origin"]);
    Op::refs_only(
        OperationKind::RenameBranch,
        "Rename 'main' to 'trunk'",
        |fx| {
            fx.git(&["branch", "-m", "main", "trunk"]);
        },
    )
}

fn rename_other_branch(fx: &Fixture) -> Op {
    fx.git(&["branch", "topic"]);
    fx.git(&["config", "branch.topic.remote", "origin"]);
    fx.git(&["config", "branch.topic.merge", "refs/heads/topic"]);
    Op::refs_only(
        OperationKind::RenameBranch,
        "Rename 'topic' to 'renamed'",
        |fx| {
            fx.git(&["branch", "-m", "topic", "renamed"]);
        },
    )
}

fn reset(kind: OperationKind, flag: &'static str) -> impl FnOnce(&Fixture) -> Op {
    move |fx| {
        fx.commit_file("b.txt", "b\n", "add b");
        if kind == OperationKind::ResetHard {
            fx.write("a.txt", "a, dirty\n");
            fx.write("junk.txt", "untracked\n");
        }
        Op::full(kind, "Reset to HEAD~1", move |fx| {
            fx.git(&["reset", "-q", flag, "HEAD~1"]);
        })
    }
}

fn merge_fast_forward(fx: &Fixture) -> Op {
    feature_branch(fx);
    Op::full(OperationKind::Merge, "Merge 'feature' into main", |fx| {
        fx.git(&["merge", "-q", "--ff-only", "feature"]);
    })
}

fn merge_no_ff(fx: &Fixture) -> Op {
    feature_branch(fx);
    fx.commit_file("c.txt", "c\n", "add c");
    Op::full(OperationKind::Merge, "Merge 'feature' into main", |fx| {
        fx.git(&[
            "merge",
            "-q",
            "--no-ff",
            "--autostash",
            "-m",
            "Merge feature",
            "feature",
        ]);
    })
}

fn rebase_clean(fx: &Fixture) -> Op {
    feature_branch(fx);
    fx.commit_file("c.txt", "c\n", "add c");
    fx.git(&["checkout", "-q", "feature"]);
    Op::full(OperationKind::Rebase, "Rebase 'feature' onto main", |fx| {
        fx.git(&["rebase", "-q", "--autostash", "main"]);
    })
}

fn rebase_interactive(fx: &Fixture) -> Op {
    fx.git(&["checkout", "-q", "-b", "feature"]);
    fx.commit_file("b1.txt", "b1\n", "b1");
    fx.commit_file("b2.txt", "b2\n", "b2");
    fx.commit_file("b3.txt", "b3\n", "b3");
    fx.git(&["checkout", "-q", "main"]);
    fx.commit_file("c.txt", "c\n", "add c");
    fx.git(&["checkout", "-q", "feature"]);
    let todo = format!(
        "reword {}\nsquash {}\ndrop {}\n",
        fx.rev("HEAD~2"),
        fx.rev("HEAD~1"),
        fx.rev("HEAD")
    );
    let todo_path = fx.tmp().join("todo.txt");
    std::fs::write(&todo_path, todo).expect("the todo can be written");
    let editor = format!("cp '{}'", slash(&todo_path));
    Op::full(
        OperationKind::RebaseInteractive,
        "Rebase 'feature' interactively onto main",
        move |fx| {
            fx.git_env(
                &["rebase", "-q", "-i", "--autostash", "main"],
                &[("GIT_SEQUENCE_EDITOR", &editor), ("GIT_EDITOR", "true")],
            );
        },
    )
}

fn cherry_pick(fx: &Fixture) -> Op {
    feature_branch(fx);
    Op::full(OperationKind::CherryPick, "Cherry-pick 'add b'", |fx| {
        fx.git(&["cherry-pick", "feature"]);
    })
}

fn revert(fx: &Fixture) -> Op {
    fx.commit_file("b.txt", "b\n", "add b");
    Op::full(OperationKind::Revert, "Revert 'add b'", |fx| {
        fx.git(&["revert", "--no-edit", "HEAD"]);
    })
}

fn stash_push_untracked(fx: &Fixture) -> Op {
    fx.write("a.txt", "a, stashed\n");
    fx.write("junk.txt", "stashed untracked\n");
    Op::full(OperationKind::StashPush, "Stash changes", |fx| {
        fx.git(&["stash", "push", "-q", "-u"]);
    })
    .untracked(UntrackedCapture::All)
}

fn stash_with_untracked(fx: &Fixture) {
    fx.write("a.txt", "a, stashed\n");
    fx.write("v.txt", "stashed untracked\n");
    fx.git(&["stash", "push", "-q", "-u"]);
}

fn stash_apply(fx: &Fixture) -> Op {
    stash_with_untracked(fx);
    Op::full(OperationKind::StashApply, "Apply stash@{0}", |fx| {
        fx.git(&["stash", "apply", "-q"]);
    })
    .untracked(stash_untracked_paths(fx))
}

fn stash_pop(fx: &Fixture) -> Op {
    stash_with_untracked(fx);
    Op::full(OperationKind::StashPop, "Pop stash@{0}", |fx| {
        fx.git(&["stash", "pop", "-q"]);
    })
    .untracked(stash_untracked_paths(fx))
}

fn two_stashes(fx: &Fixture) {
    fx.write("a.txt", "a, first stash\n");
    fx.git(&["stash", "push", "-q", "-m", "first"]);
    fx.write("a.txt", "a, second stash\n");
    fx.git(&["stash", "push", "-q", "-m", "second"]);
}

fn stash_drop(fx: &Fixture) -> Op {
    two_stashes(fx);
    Op::refs_only(OperationKind::StashDrop, "Drop stash@{1}", |fx| {
        fx.git(&["stash", "drop", "-q", "stash@{1}"]);
    })
}

fn stash_clear(fx: &Fixture) -> Op {
    two_stashes(fx);
    Op::refs_only(OperationKind::StashClear, "Clear stashes", |fx| {
        fx.git(&["stash", "clear"]);
    })
}

fn discard_file(fx: &Fixture) -> Op {
    fx.write("a.txt", "a, discarded\n");
    Op::full(OperationKind::Discard, "Discard 'a.txt'", |fx| {
        fx.git(&["checkout", "-q", "--", "a.txt"]);
    })
    .untracked(UntrackedCapture::Paths(vec![b("a.txt")]))
}

fn delete_untracked(fx: &Fixture) -> Op {
    fx.write("junk.txt", "to be deleted\n");
    Op::full(OperationKind::DeleteUntracked, "Delete 'junk.txt'", |fx| {
        std::fs::remove_file(fx.path.join("junk.txt")).expect("the file can be deleted");
    })
    .untracked(UntrackedCapture::Paths(vec![b("junk.txt")]))
}

fn discard_hunk(fx: &Fixture) -> Op {
    let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
    fx.commit_file("h.txt", &lines, "add h");
    fx.write(
        "h.txt",
        lines
            .replace("line 2\n", "line two\n")
            .replace("line 19\n", "line nineteen\n"),
    );
    Op::full(OperationKind::Discard, "Discard a hunk of 'h.txt'", |fx| {
        let diff = fx.git(&["diff", "-U0", "--", "h.txt"]);
        // Keep the header and the first hunk only.
        let mut patch = String::new();
        let mut hunks = 0;
        for line in diff.lines() {
            if line.starts_with("@@") {
                hunks += 1;
            }
            if hunks <= 1 {
                patch.push_str(line);
                patch.push('\n');
            }
        }
        let patch_path = fx.tmp().join("hunk.patch");
        std::fs::write(&patch_path, patch).expect("the patch can be written");
        fx.git(&["apply", "-R", "--unidiff-zero", &slash(&patch_path)]);
    })
    .untracked(UntrackedCapture::Paths(vec![b("h.txt")]))
}

fn tag_create(_: &Fixture) -> Op {
    Op::refs_only(OperationKind::CreateTag, "Create tag 'v1'", |fx| {
        fx.git(&["tag", "-a", "v1", "-m", "version 1"]);
    })
}

fn tag_delete_then_gc(fx: &Fixture) -> Op {
    fx.git(&["tag", "-a", "v1", "-m", "version 1", "HEAD~1"]);
    Op::refs_only(OperationKind::DeleteTag, "Delete tag 'v1'", |fx| {
        fx.git(&["tag", "-d", "v1"]);
    })
    .then(|fx| {
        fx.git(&["reflog", "expire", "--expire-unreachable=now", "--all"]);
        fx.git(&["gc", "-q", "--prune=now"]);
    })
}

fn pull(rebase: bool) -> impl FnOnce(&Fixture) -> Op {
    move |fx| {
        let remote = add_remote(fx);
        let other = other_clone(fx, &remote);
        other.commit_file("b.txt", "b from elsewhere\n", "add b elsewhere");
        other.git(&["push", "-q", "origin", "main"]);
        fx.commit_file("c.txt", "c\n", "add c");
        Op::full(OperationKind::Pull, "Pull origin/main", move |fx| {
            if rebase {
                fx.git(&["pull", "-q", "--rebase", "--autostash", "origin", "main"]);
            } else {
                fx.git(&[
                    "pull",
                    "-q",
                    "--no-rebase",
                    "--autostash",
                    "--no-edit",
                    "origin",
                    "main",
                ]);
            }
        })
    }
}

/// Generate one test per worktree condition for `$op` (a `fn(&Fixture) -> Op` expression). Operations marked
/// `without staged` get no test for condition (iv), because Git itself refuses them with a staged change
/// ("your local changes would be overwritten").
macro_rules! matrix {
    (@tests $op:expr) => {
        #[test]
        fn clean() {
            check(base_fixture(), Condition::Clean, $op);
        }

        #[test]
        fn modified_tracked() {
            check(base_fixture(), Condition::ModifiedTracked, $op);
        }

        #[test]
        fn untracked() {
            check(base_fixture(), Condition::Untracked, $op);
        }
    };
    ($($name:ident => $op:expr $(, $without:ident staged)?;)*) => {
        $(
            mod $name {
                use super::*;

                matrix!(@tests $op);
                matrix!(@staged $op $(, $without)?);
            }
        )*
    };
    (@staged $op:expr) => {
        #[test]
        fn staged() {
            check(base_fixture(), Condition::Staged, $op);
        }
    };
    (@staged $op:expr, without) => {};
}

matrix! {
    commit_op => commit;
    commit_new_and_staged_op => commit_new_and_staged;
    amend_op => amend;
    checkout_branch_carrying_changes_op => checkout_branch_carrying_changes;
    checkout_detached_op => checkout_detached;
    checkout_remote_branch_op => checkout_remote_branch;
    create_branch_op => create_branch;
    create_and_checkout_branch_op => create_and_checkout_branch;
    delete_branch_op => delete_branch;
    rename_current_branch_op => rename_current_branch;
    rename_other_branch_op => rename_other_branch;
    reset_soft_op => reset(OperationKind::ResetSoft, "--soft");
    reset_mixed_op => reset(OperationKind::ResetMixed, "--mixed");
    reset_hard_op => reset(OperationKind::ResetHard, "--hard");
    merge_fast_forward_op => merge_fast_forward;
    merge_no_ff_op => merge_no_ff;
    rebase_clean_op => rebase_clean;
    rebase_interactive_op => rebase_interactive;
    cherry_pick_op => cherry_pick, without staged;
    revert_op => revert, without staged;
    stash_push_untracked_op => stash_push_untracked;
    stash_apply_op => stash_apply;
    stash_pop_op => stash_pop;
    stash_drop_op => stash_drop;
    stash_clear_op => stash_clear;
    discard_file_op => discard_file;
    delete_untracked_op => delete_untracked;
    discard_hunk_op => discard_hunk;
    tag_create_op => tag_create;
    tag_delete_then_gc_op => tag_delete_then_gc;
    pull_merge_op => pull(false);
    pull_rebase_op => pull(true);
}

/// The first commit of an empty repository: undo makes `main` unborn again.
mod first_commit {
    use super::*;

    fn first_commit(_: &Fixture) -> Op {
        Op::full(OperationKind::Commit, "Commit \"initial\"", |fx| {
            fx.git(&["commit", "-q", "-m", "initial"]);
        })
    }

    fn empty_with_staged_file() -> Fixture {
        let fx = Fixture::empty();
        fx.write("a.txt", "a\n");
        fx.git(&["add", "a.txt"]);
        fx
    }

    #[test]
    fn clean() {
        check(empty_with_staged_file(), Condition::Clean, first_commit);
    }

    #[test]
    fn untracked() {
        check(empty_with_staged_file(), Condition::Untracked, first_commit);
    }

    #[test]
    fn modified_staged_file() {
        let fx = empty_with_staged_file();
        fx.write("a.txt", "a, modified after staging\n");
        check(fx, Condition::Clean, first_commit);
    }

    #[test]
    fn undo_leaves_main_unborn() {
        let fx = empty_with_staged_file();
        let oplog = fx.oplog();
        record_ok(&fx, &oplog, first_commit(&fx).desc, |fx| {
            fx.git(&["commit", "-q", "-m", "initial"]);
        });
        done(undo(&fx, &oplog));
        assert_eq!(
            fx.git(&["symbolic-ref", "HEAD"]).trim(),
            "refs/heads/main",
            "HEAD stays symbolic"
        );
        assert!(
            !fx.git_output(&["rev-parse", "--verify", "-q", "refs/heads/main"])
                .status
                .success(),
            "main is unborn again"
        );
        assert_eq!(
            fx.git(&["ls-files", "-s"]).lines().count(),
            1,
            "a.txt is staged"
        );
    }
}
