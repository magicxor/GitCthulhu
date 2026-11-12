use std::path::Path;

use but_core::snapshot;
use but_testsupport::{CommandExt, git_at_dir, open_repo};
use gix::prelude::ObjectIdExt;

use crate::snapshot::args_for_worktree_changes;

fn git_ok(dir: &Path, args: &[&str]) {
    git_at_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .run();
}

fn write(dir: &Path, name: &str, content: &str) -> anyhow::Result<()> {
    std::fs::write(dir.join(name), content)?;
    Ok(())
}

fn entries(index: &gix::index::State) -> Vec<(String, u32, gix::ObjectId, u32)> {
    let mut v: Vec<_> = index
        .entries()
        .iter()
        .map(|e| {
            (
                e.path(index).to_string(),
                e.flags.stage() as u32,
                e.id,
                e.mode.bits(),
            )
        })
        .collect();
    v.sort();
    v
}

/// A repository whose index holds a content conflict in `file`, plus a staged `staged.txt`.
fn conflicted_repo() -> anyhow::Result<(gix::Repository, tempfile::TempDir)> {
    let tmp = tempfile::TempDir::new()?;
    let dir = tmp.path();
    git_ok(dir, &["init", "-b", "main"]);
    write(dir, "file", "base\n")?;
    git_ok(dir, &["add", "file"]);
    git_ok(dir, &["commit", "-m", "base"]);
    git_ok(dir, &["checkout", "-b", "other"]);
    write(dir, "file", "other\n")?;
    git_ok(dir, &["commit", "-am", "other"]);
    git_ok(dir, &["checkout", "main"]);
    write(dir, "file", "main\n")?;
    git_ok(dir, &["commit", "-am", "main"]);
    // Fails with a conflict by design.
    let status = git_at_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(["merge", "other"])
        .output()?
        .status;
    assert!(!status.success(), "the merge must conflict");
    write(dir, "staged.txt", "staged\n")?;
    git_ok(dir, &["add", "staged.txt"]);
    let repo = open_repo(dir)?;
    Ok((repo, tmp))
}

#[test]
fn conflicted_index_round_trips_through_snapshot_tree() -> anyhow::Result<()> {
    let (repo, _tmp) = conflicted_repo()?;
    let original = entries(&**repo.index()?);
    assert!(
        original.iter().any(|(_, stage, ..)| *stage == 3),
        "the fixture has all conflict stages: {original:?}"
    );

    let (head_tree_id, state) = args_for_worktree_changes(&repo)?;
    let out = snapshot::create_tree(head_tree_id, state)?;
    let conflicts = out
        .index_conflicts
        .expect("conflicts are recorded in the snapshot");
    let index_tree = out.index.unwrap_or(out.head_tree);

    let (mut index, _path) = repo.index_from_tree(&index_tree)?.into_parts();
    snapshot::restore_conflicts_into_index(&mut index, conflicts.attach(&repo))?;
    assert_eq!(
        entries(&index),
        original,
        "path, stage, id and mode of each entry survive the round trip"
    );
    Ok(())
}
