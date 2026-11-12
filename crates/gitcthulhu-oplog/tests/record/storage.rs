//! A1-A3, A11, A12, A14, A15: the commit chain, reachability, transfer, concurrency and linked worktrees.

use std::{sync::Arc, time::Duration};

use gitcthulhu_oplog::{
    EntryId, EntryResult, EntryRole, EntryStatus, OperationKind, PageRequest, Scope,
};

use crate::support::{
    Fixture, all_entries, git_in, ls_tree, record_branch, record_commit, record_ok, refs_only,
    show_json,
};

const MAIN_REF: &str = "refs/gitcthulhu/oplog/main";

#[test]
fn first_finish_creates_the_ref_with_the_documented_layout() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    assert_eq!(
        oplog.ref_name().as_bstr(),
        MAIN_REF,
        "the main worktree uses the main chain"
    );
    let before_head = fx.rev("HEAD");
    let id = record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    let after_head = fx.rev("HEAD");

    let parents = fx.git(&["rev-list", "--parents", "-n1", MAIN_REF]);
    let mut parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
    parents.sort_unstable();
    let mut kept = vec![before_head.as_str(), after_head.as_str()];
    kept.sort_unstable();
    assert_eq!(
        parents, kept,
        "the first entry has no previous entry, only the commits main moved away from and onto"
    );

    let message = fx.git(&["log", "-1", "--format=%B", MAIN_REF]);
    assert!(
        message.starts_with("Commit \"add b\"\n"),
        "the message starts with the title: {message:?}"
    );
    assert!(
        message.contains(&format!("GitCthulhu-Oplog: {} commit", id.0)),
        "the trailer names sequence number and kind: {message:?}"
    );

    // meta.json, both snapshots (refs as gitlinks, state.json, the three file trees, which `-r` descends into).
    snapbox::assert_data_eq!(
        ls_tree(&fx, MAIN_REF),
        snapbox::str![[r#"
after/files/head-tree/a.txt
after/files/head-tree/b.txt
after/files/index/a.txt
after/files/index/b.txt
after/files/worktree/a.txt
after/files/worktree/b.txt
after/refs/heads/main
after/state.json
before/files/head-tree/a.txt
before/files/index/a.txt
before/files/worktree/a.txt
before/refs/heads/main
before/state.json
meta.json

"#]]
    );

    let meta = show_json(&fx, &format!("{MAIN_REF}:meta.json"));
    assert_eq!(meta["version"], 1, "format version 1");
    assert_eq!(meta["seq"], id.0, "seq is the entry id");
    assert_eq!(meta["kind"], "commit", "kebab-case kind");
    assert_eq!(
        meta["role"],
        serde_json::json!({"type": "operation", "undoable": true}),
        "role is tagged by type"
    );
    assert_eq!(
        meta["refsChanged"],
        serde_json::json!(["refs/heads/main"]),
        "only main moved"
    );
    assert_eq!(meta["result"], "succeeded");
    assert_eq!(meta["afterCaptured"], true);
    assert_eq!(meta["untracked"], "none");

    let entry = oplog
        .entry(&fx.repo(), id)
        .expect("reading works")
        .expect("the entry exists");
    assert_eq!(entry.title, "Commit \"add b\"");
    assert_eq!(entry.kind, OperationKind::Commit);
    assert_eq!(entry.role, EntryRole::Operation { undoable: true });
    assert_eq!(entry.scope, Scope::Full);
    assert_eq!(entry.result, EntryResult::Succeeded);
    assert_eq!(entry.status, EntryStatus::Undoable { next: true });
    assert_eq!(entry.refs_changed.len(), 1);
    assert_eq!(entry.refs_changed[0].as_bstr(), "refs/heads/main");
    assert!(entry.restorable, "written by this version");
    assert_eq!(entry.commit.to_string(), fx.rev(MAIN_REF));
}

#[test]
fn undone_commit_survives_reflog_expiry_and_gc() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    let undone = fx.rev("HEAD");
    fx.git(&["reset", "-q", "--hard", "HEAD~"]);
    fx.git(&["reflog", "expire", "--expire-unreachable=now", "--all"]);
    fx.git(&["gc", "-q", "--prune=now"]);
    assert!(
        fx.git_output(&["cat-file", "-e", &undone]).status.success(),
        "the commit is an extra parent of the entry commit, so gc keeps it"
    );
}

#[test]
fn deleted_annotated_tag_object_is_kept_as_raw_blob() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.git(&["tag", "-a", "v1", "-m", "release 1"]);
    let tag = fx.rev("refs/tags/v1");
    let raw = fx.git(&["cat-file", "tag", &tag]);
    record_ok(
        &fx,
        &oplog,
        refs_only(OperationKind::DeleteTag, "Delete tag 'v1'"),
        |fx| {
            fx.git(&["tag", "-d", "v1"]);
        },
    );
    fx.git(&["reflog", "expire", "--expire-unreachable=now", "--all"]);
    fx.git(&["gc", "-q", "--prune=now"]);
    let kept = fx.git(&["show", &format!("{MAIN_REF}:keep-tags/{tag}")]);
    assert_eq!(
        kept, raw,
        "the raw tag object is stored under keep-tags/<hex>"
    );
}

#[test]
fn oplog_ref_is_not_transferred_by_clone_push_or_fetch() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    fx.git(&["tag", "v1"]);

    let tmp = fx.tmp().to_owned();
    let repo_path = fx.path.to_str().expect("UTF-8 temp path").to_owned();
    git_in(&tmp, &["clone", "-q", &repo_path, "clone"]);
    let clone = tmp.join("clone");
    assert_eq!(
        git_in(&clone, &["for-each-ref", "refs/gitcthulhu"]),
        "",
        "clone copies heads and tags only"
    );

    git_in(&tmp, &["init", "-q", "--bare", "remote.git"]);
    let remote = tmp.join("remote.git");
    let remote_str = remote.to_str().expect("UTF-8 temp path");
    fx.git(&["push", "-q", remote_str, "--all"]);
    fx.git(&["push", "-q", remote_str, "--tags"]);
    assert_eq!(
        git_in(&remote, &["for-each-ref", "refs/gitcthulhu"]),
        "",
        "push --all and --tags push heads and tags only"
    );

    record_commit(&fx, &oplog, "c.txt", "c\n", "add c");
    git_in(&clone, &["fetch", "-q", "origin"]);
    assert_eq!(
        git_in(&clone, &["for-each-ref", "refs/gitcthulhu"]),
        "",
        "the default fetch refspec covers refs/heads only"
    );
}

#[test]
fn mirror_clone_copies_the_ref_but_never_uncaptured_untracked_contents() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    fx.write(".env", "SECRET=hunter2\n");
    record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    assert!(
        !crate::support::has_blob_of(&fx, b"SECRET=hunter2\n"),
        "a commit captures no untracked contents"
    );

    let tmp = fx.tmp().to_owned();
    let repo_path = fx.path.to_str().expect("UTF-8 temp path").to_owned();
    git_in(&tmp, &["clone", "-q", "--mirror", &repo_path, "mirror.git"]);
    let mirror = tmp.join("mirror.git");
    assert_eq!(
        git_in(
            &mirror,
            &["for-each-ref", "--format=%(refname)", "refs/gitcthulhu"]
        ),
        format!("{MAIN_REF}\n"),
        "a mirror clone does copy the oplog ref (documented)"
    );
    let objects = git_in(&mirror, &["cat-file", "--batch-all-objects", "--batch"]);
    assert!(
        !objects.contains("hunter2"),
        "no object in the mirror holds the untracked secret"
    );
}

#[test]
fn concurrent_appends_from_two_threads_form_a_linear_chain() {
    let fx = Fixture::new();
    let oplog = Arc::new(fx.oplog());
    let head = fx.rev("HEAD");
    let head = gix::ObjectId::from_hex(head.as_bytes()).expect("valid id");
    let threads: Vec<_> = (0..2)
        .map(|t| {
            let oplog = Arc::clone(&oplog);
            let path = fx.path.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    let repo = but_testsupport::open_repo(&path).expect("repo opens");
                    let mut guard = but_core::sync::exclusive_repo_access(repo.common_dir(), None);
                    let pending = oplog
                        .prepare(
                            &repo,
                            refs_only(OperationKind::CreateBranch, &format!("t{t}-{i}")),
                            guard.write_permission(),
                        )
                        .expect("prepare works")
                        .expect("recording is enabled");
                    repo.reference(
                        format!("refs/heads/t{t}-{i}"),
                        head,
                        gix::refs::transaction::PreviousValue::MustNotExist,
                        "test",
                    )
                    .expect("the branch can be created");
                    oplog
                        .finish(
                            &repo,
                            pending,
                            gitcthulhu_oplog::OperationResult::Succeeded,
                            guard.write_permission(),
                        )
                        .expect("finish works")
                        .expect("a branch was created");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("the thread did not panic");
    }
    let entries = all_entries(&fx, &oplog);
    assert_eq!(entries.len(), 100, "every operation was recorded");
    for pair in entries.windows(2) {
        assert_eq!(
            pair[1].id.0,
            pair[0].id.0 + 1,
            "sequence numbers strictly increase by one"
        );
    }
    let first_parents = fx.git(&["rev-list", "--first-parent", MAIN_REF]);
    let first_parents: Vec<&str> = first_parents.lines().take(entries.len()).collect();
    let newest_first: Vec<String> = entries.iter().rev().map(|e| e.commit.to_string()).collect();
    assert_eq!(
        first_parents, newest_first,
        "each entry commit's first parent is the previous entry"
    );
}

#[test]
fn clear_removes_the_ref_and_the_next_chain_starts_with_a_larger_seq() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let first = record_branch(&fx, &oplog, "one");
    {
        let mut guard = fx.guard();
        oplog
            .clear(&fx.repo(), guard.write_permission())
            .expect("clearing works");
    }
    assert_eq!(
        fx.git(&["for-each-ref", "refs/gitcthulhu"]),
        "",
        "the ref is gone"
    );
    assert!(
        !oplog.can_undo(&fx.repo()).expect("reading works"),
        "nothing to undo"
    );
    let second = record_branch(&fx, &oplog, "two");
    assert!(
        second.0 > first.0,
        "{second:?} > {first:?}: ids never repeat after clear"
    );
    assert_eq!(all_entries(&fx, &oplog).len(), 1, "a new chain");
}

#[test]
fn reads_do_not_wait_for_the_exclusive_guard() {
    let fx = Fixture::new();
    let oplog = Arc::new(fx.oplog());
    let id = record_commit(&fx, &oplog, "b.txt", "b\n", "add b");

    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let path = fx.path.clone();
    let holder = std::thread::spawn(move || {
        let repo = but_testsupport::open_repo(&path).expect("repo opens");
        let _guard = but_core::sync::exclusive_repo_access(repo.common_dir(), None);
        locked_tx.send(()).expect("the test waits");
        release_rx.recv().ok();
    });
    locked_rx.recv().expect("the guard is held");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let reader = {
        let oplog = Arc::clone(&oplog);
        let path = fx.path.clone();
        std::thread::spawn(move || {
            let repo = but_testsupport::open_repo(&path).expect("repo opens");
            let state = oplog.state(&repo).expect("state works");
            let page = oplog
                .list(
                    &repo,
                    PageRequest {
                        before: None,
                        limit: 10,
                        include_bookkeeping: false,
                    },
                )
                .expect("list works");
            let entry = oplog.entry(&repo, id).expect("entry works");
            done_tx
                .send((
                    state.undo.map(|u| u.operation),
                    page.entries.len(),
                    entry.is_some(),
                ))
                .expect("the test waits");
        })
    };
    let result = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("reads return while another thread holds the exclusive guard");
    assert_eq!(result, (Some(id), 1, true));
    release_tx.send(()).expect("the holder waits");
    holder.join().expect("no panic");
    reader.join().expect("no panic");
    assert!(
        oplog.can_undo(&fx.repo()).expect("reading works"),
        "reading again after the guard was released sees the same state"
    );
}

#[test]
fn linked_worktree_has_its_own_chain_which_can_be_cleared() {
    let fx = Fixture::new();
    let wt_path = fx.tmp().join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        wt_path.to_str().expect("UTF-8"),
        "-b",
        "feature",
    ]);
    let wt = fx.worktree(wt_path);
    let oplog = wt.oplog();
    assert_eq!(
        oplog.ref_name().as_bstr(),
        "refs/gitcthulhu/oplog/linked/wt",
        "the key is the linked worktree's id"
    );
    record_commit(&wt, &oplog, "w.txt", "w\n", "in worktree");
    assert_eq!(
        fx.git(&["for-each-ref", "--format=%(refname)", "refs/gitcthulhu"]),
        "refs/gitcthulhu/oplog/linked/wt\n",
        "refs are shared, so the main worktree sees the linked chain's ref"
    );
    assert!(
        !fx.oplog().can_undo(&fx.repo()).expect("reading works"),
        "the main chain is unaffected"
    );
    assert!(oplog.can_undo(&wt.repo()).expect("reading works"));

    let mut guard = fx.guard();
    gitcthulhu_oplog::Oplog::clear_linked_worktree(&fx.repo(), "wt", guard.write_permission())
        .expect("clearing works");
    assert_eq!(
        fx.git(&["for-each-ref", "refs/gitcthulhu"]),
        "",
        "the linked chain is gone"
    );
    gitcthulhu_oplog::Oplog::clear_linked_worktree(&fx.repo(), "wt", guard.write_permission())
        .expect("a missing ref is fine");
}

#[test]
fn reused_linked_worktree_id_at_another_path_replays_to_empty_stacks() {
    let fx = Fixture::new();
    let path_a = fx.tmp().join("a").join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        path_a.to_str().expect("UTF-8"),
        "-b",
        "one",
    ]);
    let old_id = {
        let wt = fx.worktree(path_a.clone());
        let oplog = wt.oplog();
        let id = record_commit(&wt, &oplog, "w.txt", "w\n", "in worktree a");
        assert!(oplog.can_undo(&wt.repo()).expect("reading works"));
        id
    };
    fx.git(&["worktree", "remove", path_a.to_str().expect("UTF-8")]);

    let path_b = fx.tmp().join("b").join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        path_b.to_str().expect("UTF-8"),
        "-b",
        "two",
    ]);
    let wt = fx.worktree(path_b);
    let oplog = wt.oplog();
    assert_eq!(
        oplog.ref_name().as_bstr(),
        "refs/gitcthulhu/oplog/linked/wt",
        "the id was reused"
    );
    assert!(
        !oplog.can_undo(&wt.repo()).expect("reading works"),
        "entries written at another path never undo into the new worktree"
    );
    let entry = oplog
        .entry(&wt.repo(), old_id)
        .expect("reading works")
        .expect("the old entry is still listed");
    assert_eq!(entry.status, EntryStatus::NotUndoable);
}

#[test]
fn main_chain_survives_moving_the_repository_directory() {
    let fx = Fixture::new();
    {
        let oplog = fx.oplog();
        record_commit(&fx, &oplog, "b.txt", "b\n", "add b");
    }
    let moved = fx.tmp().join("moved");
    std::fs::rename(&fx.path, &moved).expect("the repository can be moved");
    let fx = fx.worktree(moved);
    let oplog = fx.oplog();
    assert!(
        oplog.can_undo(&fx.repo()).expect("reading works"),
        "the main chain ignores worktree_path"
    );
}

/// Append an entry commit with the raw `meta_json` on top of the main chain, as another writer (a newer
/// GitCthulhu, or a corrupting tool) would, and return its id.
fn append_raw_entry(fx: &Fixture, meta_json: &[u8], seq: u64) -> gix::ObjectId {
    let repo = fx.repo();
    let blob = repo
        .write_blob(meta_json)
        .expect("the blob can be written")
        .detach();
    let mut editor = repo.empty_tree().edit().expect("an editor can be created");
    editor
        .upsert("meta.json", gix::object::tree::EntryKind::Blob, blob)
        .expect("the entry can be added");
    let tree = editor.write().expect("the tree can be written").detach();
    let previous = gix::ObjectId::from_hex(fx.rev(MAIN_REF).as_bytes()).expect("valid id");
    let signature = gix::actor::Signature {
        name: "GitCthulhu".into(),
        email: "gitcthulhu@localhost".into(),
        time: gix::date::Time {
            seconds: 2,
            offset: 0,
        },
    };
    let commit = repo
        .write_object(&gix::objs::Commit {
            tree,
            parents: vec![previous].into(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: format!(
                "Raw entry

GitCthulhu-Oplog: {seq} unknown
"
            )
            .into(),
            extra_headers: Vec::new(),
        })
        .expect("the commit can be written")
        .detach();
    repo.reference(
        MAIN_REF,
        commit,
        gix::refs::transaction::PreviousValue::MustExistAndMatch(previous.into()),
        "raw entry",
    )
    .expect("the oplog ref can be moved");
    commit
}

#[test]
fn entries_of_a_newer_format_version_are_listed_but_act_as_barriers() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let first = record_commit(
        &fx, &oplog, "b.txt", "b
", "add b",
    );

    // A newer GitCthulhu appended an entry with a role, scope and result this build does not know.
    let newer = EntryId(first.0 + 1);
    let meta = serde_json::json!({
        "version": 2,
        "seq": newer.0,
        "role": {"type": "squash", "into": first.0},
        "kind": "future-kind",
        "title": "From the future",
        "scope": "partial",
        "startedAtMs": 1,
        "finishedAtMs": 2,
        "result": {"retried": 3},
        "worktreePath": "C:/elsewhere",
        "appVersion": "9.0.0",
    });
    append_raw_entry(
        &fx,
        &serde_json::to_vec(&meta).expect("valid JSON"),
        newer.0,
    );

    let entry = oplog
        .entry(&fx.repo(), newer)
        .expect("the chain stays readable")
        .expect("the newer entry is listed");
    assert!(!entry.restorable, "written by a newer format version");
    assert_eq!(
        entry.kind,
        OperationKind::Unknown,
        "unknown kind strings map to Unknown"
    );
    assert_eq!(entry.title, "From the future", "the title is still shown");
    assert_eq!(entry.status, EntryStatus::NotUndoable);
    assert!(
        !oplog.can_undo(&fx.repo()).expect("reading works"),
        "nothing before an entry this build cannot interpret is undone"
    );

    let next = record_commit(
        &fx, &oplog, "c.txt", "c
", "add c",
    );
    assert_eq!(next.0, newer.0 + 1, "appending continues the chain");
    assert_eq!(
        oplog
            .state(&fx.repo())
            .expect("reading works")
            .undo
            .expect("the new commit is undoable")
            .operation,
        next,
        "only the entry after the barrier is undoable"
    );
}

#[test]
fn unparsable_metadata_of_the_current_version_is_reported_as_corrupt() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    let first = record_commit(
        &fx, &oplog, "b.txt", "b
", "add b",
    );
    let seq = first.0 + 1;
    let broken = append_raw_entry(
        &fx,
        format!(r#"{{"version": 1, "seq": {seq}, "role": {{"type": "squash"}}}}"#).as_bytes(),
        seq,
    );
    assert!(
        matches!(
            oplog.state(&fx.repo()),
            Err(gitcthulhu_oplog::OplogError::Corrupt { commit, .. }) if commit == broken
        ),
        "metadata of a known format version must parse"
    );
}
