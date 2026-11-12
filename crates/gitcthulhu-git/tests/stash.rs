//! Tests of `stash::*` (spec §2.6, WP-B6).

mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, GitError, GitErrorKind, NoPrompter, Repo, StashApplyOptions, StashApplyResult,
    StashPushOptions, stash,
};
use gix::ObjectId;
use support::{TestEnv, TestRepo};

fn assert_kind<T: std::fmt::Debug>(result: Result<T, GitError>, expected: GitErrorKind, why: &str) {
    match result {
        Err(err) => assert_eq!(err.kind(), expected, "{why}: got {err:?}"),
        Ok(value) => panic!("{why}: expected {expected:?}, got Ok({value:?})"),
    }
}

fn with_perm<T>(
    fixture: &TestRepo,
    repo: &Repo,
    op: impl FnOnce(&mut gitcthulhu_git::RepoExclusive, &gitcthulhu_git::OpContext) -> T,
) -> T {
    let ctx = fixture.op_ctx(CancelToken::new(), Arc::new(NoPrompter));
    let mut guard = fixture.exclusive(repo);
    op(guard.write_permission(), &ctx)
}

fn push(
    fixture: &TestRepo,
    repo: &Repo,
    opts: &StashPushOptions,
) -> Result<Option<ObjectId>, GitError> {
    with_perm(fixture, repo, |perm, ctx| {
        stash::stash_push(repo, perm, ctx, opts)
    })
    .map(|outcome| outcome.value)
}

fn id_of(fixture: &TestRepo, rev: &str) -> ObjectId {
    ObjectId::from_hex(fixture.git(&["rev-parse", rev]).as_bytes()).expect("hex id")
}

/// Two committed files `a.txt` and `b.txt`.
fn base(env: &TestEnv) -> TestRepo {
    let fixture = env.repo("repo");
    fixture.write("a.txt", "a\n");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("initial");
    fixture
}

fn stash_count(fixture: &TestRepo) -> usize {
    fixture.git(&["stash", "list"]).lines().count()
}

#[test]
fn push_with_message_and_untracked() {
    let env = TestEnv::new();
    let fixture = base(&env);
    fixture.write("a.txt", "changed\n");
    fixture.write("dir/new.txt", "new\n");
    let repo = fixture.open();
    let id = push(
        &fixture,
        &repo,
        &StashPushOptions {
            message: Some("work in progress".into()),
            include_untracked: true,
            ..StashPushOptions::default()
        },
    )
    .expect("stash push works")
    .expect("there was something to stash");
    assert_eq!(
        id,
        id_of(&fixture, "refs/stash"),
        "the id is the new refs/stash"
    );
    assert!(
        fixture
            .git(&["stash", "list"])
            .contains("On main: work in progress"),
        "message recorded"
    );
    assert!(
        !fixture.path.join("dir").join("new.txt").exists(),
        "the untracked file was stashed away"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "a\n",
        "the tracked change was stashed"
    );
}

#[test]
fn push_nothing_returns_none() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    let result = push(&fixture, &repo, &StashPushOptions::default()).expect("not an error");
    assert_eq!(result, None, "nothing to stash");
    assert_eq!(stash_count(&fixture), 0, "no entry created");
}

#[test]
fn keep_index() {
    let env = TestEnv::new();
    let fixture = base(&env);
    fixture.write("a.txt", "staged\n");
    fixture.git(&["add", "a.txt"]);
    fixture.write("b.txt", "unstaged\n");
    let repo = fixture.open();
    push(
        &fixture,
        &repo,
        &StashPushOptions {
            keep_index: true,
            ..StashPushOptions::default()
        },
    )
    .expect("stash push works")
    .expect("something was stashed");
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        "a.txt",
        "the staged change is still in the index"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("b.txt")),
        "b\n",
        "the unstaged change was stashed"
    );
}

#[test]
fn push_paths_only() {
    let env = TestEnv::new();
    let fixture = base(&env);
    fixture.write("a.txt", "A\n");
    fixture.write("b.txt", "B\n");
    let repo = fixture.open();
    push(
        &fixture,
        &repo,
        &StashPushOptions {
            paths: vec!["a.txt".into()],
            ..StashPushOptions::default()
        },
    )
    .expect("stash push works")
    .expect("something was stashed");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "a\n",
        "the listed path was stashed"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("b.txt")),
        "B\n",
        "other paths are untouched"
    );
}

#[test]
fn apply_by_id_after_list_shift() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    fixture.write("a.txt", "first\n");
    let first = push(&fixture, &repo, &StashPushOptions::default())
        .expect("push works")
        .expect("stashed");
    fixture.write("b.txt", "second\n");
    let second = push(&fixture, &repo, &StashPushOptions::default())
        .expect("push works")
        .expect("stashed");
    assert_eq!(stash_count(&fixture), 2, "two entries");

    // `first` is stash@{1} now; applying by id must pick the right entry.
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_apply(&repo, perm, ctx, first, StashApplyOptions::default())
    })
    .expect("apply works");
    assert!(
        matches!(outcome.value, StashApplyResult::Applied),
        "clean apply: {:?}",
        outcome.value
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "first\n",
        "the first stash was applied"
    );
    fixture.git(&["checkout", "--", "a.txt"]);

    // Dropping the newer entry shifts `first` to stash@{0}.
    with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_drop(&repo, perm, ctx, second)
    })
    .expect("drop works");
    with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_pop(&repo, perm, ctx, first, StashApplyOptions::default())
    })
    .expect("pop works after the shift");
    assert_eq!(stash_count(&fixture), 0, "pop removed the entry");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "first\n",
        "content restored"
    );
}

#[test]
fn apply_restore_index() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    fixture.write("a.txt", "staged\n");
    fixture.git(&["add", "a.txt"]);
    let id = push(&fixture, &repo, &StashPushOptions::default())
        .expect("push works")
        .expect("stashed");
    with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_apply(
            &repo,
            perm,
            ctx,
            id,
            StashApplyOptions {
                restore_index: true,
            },
        )
    })
    .expect("apply works");
    assert_eq!(
        fixture.git(&["diff", "--cached", "--name-only"]),
        "a.txt",
        "the change is staged again"
    );
    assert_eq!(stash_count(&fixture), 1, "apply keeps the entry");
}

#[test]
fn pop_conflict_keeps_entry_and_is_ok() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    fixture.write("a.txt", "stash side\n");
    let id = push(&fixture, &repo, &StashPushOptions::default())
        .expect("push works")
        .expect("stashed");
    fixture.write("a.txt", "head side\n");
    fixture.commit_all("conflicting change");
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_pop(&repo, perm, ctx, id, StashApplyOptions::default())
    })
    .expect("conflicts are not an error");
    match outcome.value {
        StashApplyResult::Conflicts { paths } => {
            assert_eq!(paths, vec!["a.txt"], "the conflicted path is reported")
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
    assert_eq!(stash_count(&fixture), 1, "Git keeps the entry on conflicts");
}

#[test]
fn apply_with_existing_untracked_is_partial() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    fixture.write("a.txt", "tracked change\n");
    fixture.write("untr", "from stash\n");
    let id = push(
        &fixture,
        &repo,
        &StashPushOptions {
            include_untracked: true,
            ..StashPushOptions::default()
        },
    )
    .expect("push works")
    .expect("stashed");
    fixture.write("untr", "recreated\n");
    let outcome = with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_apply(&repo, perm, ctx, id, StashApplyOptions::default())
    })
    .expect("a partial restore is not an error");
    match outcome.value {
        StashApplyResult::Partial {
            untracked_not_restored,
            ..
        } => assert_eq!(
            untracked_not_restored,
            vec!["untr"],
            "the existing untracked file was not overwritten"
        ),
        other => panic!("expected Partial, got {other:?}"),
    }
    assert_eq!(stash_count(&fixture), 1, "the entry is kept");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("untr")),
        "recreated\n",
        "the user's file is intact"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "tracked change\n",
        "the tracked part of the stash was applied"
    );
}

#[test]
fn drop_and_clear() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    let mut ids = Vec::new();
    for round in 0..3 {
        fixture.write("a.txt", format!("round {round}\n"));
        ids.push(
            push(&fixture, &repo, &StashPushOptions::default())
                .expect("push works")
                .expect("stashed"),
        );
    }
    assert_eq!(stash_count(&fixture), 3, "three entries");
    with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_drop(&repo, perm, ctx, ids[1])
    })
    .expect("drop works");
    assert_eq!(stash_count(&fixture), 2, "one entry less");
    assert_ne!(
        id_of(&fixture, "stash@{1}"),
        ids[1],
        "the dropped entry is gone"
    );
    with_perm(&fixture, &repo, |perm, ctx| {
        stash::stash_clear(&repo, perm, ctx)
    })
    .expect("clear works");
    assert_eq!(stash_count(&fixture), 0, "the stash is empty");
}

#[test]
fn stash_not_found() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    let bogus = ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("hex id");
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            stash::stash_apply(&repo, perm, ctx, bogus, StashApplyOptions::default())
        }),
        GitErrorKind::StashNotFound,
        "no stash at all",
    );
    fixture.write("a.txt", "x\n");
    push(&fixture, &repo, &StashPushOptions::default()).expect("push works");
    assert_kind(
        with_perm(&fixture, &repo, |perm, ctx| {
            stash::stash_drop(&repo, perm, ctx, bogus)
        }),
        GitErrorKind::StashNotFound,
        "an id that is not in the list",
    );
}

#[test]
fn untracked_paths_lists_parent3_tree() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    fixture.write("a.txt", "changed\n");
    fixture.write("z.txt", "z\n");
    fixture.write("deep/er/y.txt", "y\n");
    let with_untracked = push(
        &fixture,
        &repo,
        &StashPushOptions {
            include_untracked: true,
            ..StashPushOptions::default()
        },
    )
    .expect("push works")
    .expect("stashed");
    let paths = stash::untracked_paths(&repo, with_untracked).expect("the paths can be listed");
    assert_eq!(
        paths,
        vec!["deep/er/y.txt", "z.txt"],
        "all untracked files, sorted, no tracked ones"
    );

    fixture.write("a.txt", "changed again\n");
    let without = push(&fixture, &repo, &StashPushOptions::default())
        .expect("push works")
        .expect("stashed");
    assert!(
        stash::untracked_paths(&repo, without)
            .expect("listing works")
            .is_empty(),
        "no untracked parent, no paths"
    );
}

#[test]
fn staged_only_stash() {
    let env = TestEnv::new();
    let fixture = base(&env);
    let repo = fixture.open();
    if repo.runtime().version()
        < (gitcthulhu_git::GitVersion {
            major: 2,
            minor: 35,
            patch: 0,
        })
    {
        return;
    }
    fixture.write("a.txt", "staged\n");
    fixture.git(&["add", "a.txt"]);
    fixture.write("b.txt", "unstaged\n");
    push(
        &fixture,
        &repo,
        &StashPushOptions {
            staged_only: true,
            ..StashPushOptions::default()
        },
    )
    .expect("--staged works")
    .expect("stashed");
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("b.txt")),
        "unstaged\n",
        "the unstaged change stays"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.read("a.txt")),
        "a\n",
        "the staged change was stashed"
    );
}
