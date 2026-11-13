use std::path::Path;

use but_core::{
    RepositoryExt as _,
    worktree::{checkout, safe_checkout_from_head},
};
use but_testsupport::{CommandExt, git_at_dir, open_repo};

use crate::worktree::utils::commit_files;

fn git_ok(dir: &Path, args: &[&str]) {
    git_at_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .run();
}

fn repo_with_commit() -> anyhow::Result<(gix::Repository, tempfile::TempDir)> {
    let tmp = tempfile::TempDir::new()?;
    let dir = tmp.path();
    git_ok(dir, &["init", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n")?;
    git_ok(dir, &["add", "a.txt"]);
    git_ok(dir, &["commit", "-m", "init"]);
    let repo = open_repo(dir)?;
    Ok((repo, tmp))
}

fn blob_id(repo: &gix::Repository, content: &[u8]) -> gix::ObjectId {
    gix::objs::compute_hash(repo.object_hash(), gix::object::Kind::Blob, content)
        .expect("hashing in-memory data works")
}

const BIG: usize = 4096;

fn big_content() -> Vec<u8> {
    vec![b'x'; BIG]
}

fn limited(limit: Option<u64>) -> checkout::Options {
    checkout::Options {
        untracked_size_limit: limit,
        ..Default::default()
    }
}

#[test]
fn big_untracked_file_is_not_hashed_and_left_alone() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let content = big_content();
    std::fs::write(tmp.path().join("big.bin"), &content)?;
    let target = commit_files(&repo, "change a", &[("a.txt", Some("changed\n"))])?;

    safe_checkout_from_head(target, &repo, limited(Some(1024)))?;

    assert!(
        !repo.has_object(blob_id(&repo, &content)),
        "the oversized untracked file was never hashed"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("big.bin"))?,
        content,
        "the unknown file is never removed"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt"))?,
        "changed\n",
        "the rest of the checkout happened"
    );
    Ok(())
}

#[test]
fn without_limit_untracked_files_are_hashed() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let content = big_content();
    std::fs::write(tmp.path().join("big.bin"), &content)?;
    let target = commit_files(&repo, "change a", &[("a.txt", Some("changed\n"))])?;

    safe_checkout_from_head(target, &repo, limited(None))?;
    assert!(
        repo.has_object(blob_id(&repo, &content)),
        "current behaviour: everything is hashed"
    );
    Ok(())
}

#[test]
fn target_writing_over_big_untracked_file_is_refused_with_paths() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let content = big_content();
    std::fs::write(tmp.path().join("big.bin"), &content)?;
    let target = commit_files(&repo, "add big", &[("big.bin", Some("target content\n"))])?;

    let err = safe_checkout_from_head(target, &repo, limited(Some(1024)))
        .expect_err("the unknown file is in the way");
    let refusal = err
        .downcast_ref::<checkout::UncommittedChangesInTheWay>()
        .unwrap_or_else(|| panic!("typed refusal, got {err:?}"));
    assert_eq!(
        refusal.paths,
        vec![bstr::BString::from("big.bin")],
        "the refusal names the file that is in the way"
    );
    assert!(
        !repo.has_object(blob_id(&repo, &content)),
        "the oversized untracked file was never hashed"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("big.bin"))?,
        content,
        "the file is untouched"
    );
    Ok(())
}

#[test]
fn merge_conflict_refusal_has_paths() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    std::fs::write(tmp.path().join("a.txt"), "local edit\n")?;
    let target = commit_files(&repo, "change a", &[("a.txt", Some("remote edit\n"))])?;

    let err = safe_checkout_from_head(target, &repo, Default::default())
        .expect_err("conflicting local change");
    let refusal = err
        .downcast_ref::<checkout::UncommittedChangesInTheWay>()
        .unwrap_or_else(|| panic!("typed refusal, got {err:?}"));
    assert_eq!(
        refusal.paths,
        vec![bstr::BString::from("a.txt")],
        "the refusal of the 3-way merge names the conflicting file"
    );
    // The message keeps its quoted form, while `paths` holds the plain path.
    snapbox::assert_data_eq!(
        err.to_string(),
        snapbox::str![[r#"Uncommitted files would be overwritten by checkout: "a.txt""#]]
    );
    Ok(())
}

#[test]
#[expect(deprecated)]
fn create_wd_tree_limit_only_applies_to_untracked_files() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let content = big_content();
    std::fs::write(tmp.path().join("a.txt"), &content)?;
    std::fs::write(tmp.path().join("big.bin"), &content)?;

    let tree_id = repo.create_wd_tree(1024)?;
    let tree = repo.find_tree(tree_id)?;
    let a = tree
        .find_entry("a.txt")
        .expect("modified tracked file is kept");
    assert_eq!(
        a.object_id(),
        blob_id(&repo, &content),
        "the tracked file's new content is used even though it exceeds the limit"
    );
    assert!(repo.has_object(a.object_id()), "its blob was written");
    assert!(
        tree.find_entry("big.bin").is_none(),
        "big untracked file is skipped"
    );
    Ok(())
}

#[test]
#[expect(deprecated)]
fn create_wd_tree_limit_is_inclusive_and_ignores_directories() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let at_limit = vec![b'y'; 1024];
    std::fs::write(tmp.path().join("at-limit.bin"), &at_limit)?;
    let nested = tmp.path().join("nested");
    std::fs::create_dir(&nested)?;
    git_ok(&nested, &["init", "-b", "main"]);
    std::fs::write(nested.join("n.txt"), "n\n")?;
    git_ok(&nested, &["add", "n.txt"]);
    git_ok(&nested, &["commit", "-m", "nested"]);

    let tree_id = repo.create_wd_tree(1024)?;
    let tree = repo.find_tree(tree_id)?;
    let entry = tree
        .find_entry("at-limit.bin")
        .expect("a file of exactly the limit is not too large");
    assert_eq!(
        entry.object_id(),
        blob_id(&repo, &at_limit),
        "its content is captured"
    );
    let nested_entry = tree
        .find_entry("nested")
        .expect("the size limit applies to files, never to an untracked nested repository");
    assert_eq!(
        nested_entry.mode().kind(),
        gix::object::tree::EntryKind::Commit,
        "the nested repository is recorded by its HEAD commit, as without a limit"
    );
    Ok(())
}

#[test]
fn nested_big_untracked_files_in_the_way_are_all_listed() -> anyhow::Result<()> {
    let (repo, tmp) = repo_with_commit()?;
    let content = big_content();
    std::fs::create_dir_all(tmp.path().join("n/sub"))?;
    std::fs::write(tmp.path().join("n/sub/local"), &content)?;
    std::fs::write(tmp.path().join("n/local"), &content)?;
    let target = commit_files(&repo, "add file n", &[("n", Some("theirs\n"))])?;

    let err = safe_checkout_from_head(target, &repo, limited(Some(1024)))
        .expect_err("the unknown files are in the way of the file the target writes");
    let refusal = err
        .downcast_ref::<checkout::UncommittedChangesInTheWay>()
        .unwrap_or_else(|| panic!("typed refusal, got {err:?}"));
    assert_eq!(
        refusal.paths,
        vec![
            bstr::BString::from("n/local"),
            bstr::BString::from("n/sub/local")
        ],
        "all paths are listed, sorted, unquoted and with `/` separators on every platform"
    );
    // The message text is the one the checkout always produced.
    snapbox::assert_data_eq!(
        err.to_string(),
        snapbox::str![[
            r#"Uncommitted files would be overwritten by checkout: "n/local", "n/sub/local""#
        ]]
    );
    assert_eq!(
        std::fs::read(tmp.path().join("n/sub/local"))?,
        content,
        "the files are untouched"
    );
    Ok(())
}
