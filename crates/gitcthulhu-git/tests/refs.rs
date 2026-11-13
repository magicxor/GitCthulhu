//! Tests of `refs` (spec §2.5, WP-B5).

mod support;

use gitcthulhu_git::{CancelToken, GitErrorKind, HeadState, RefLabel, RefListOptions, Repo, refs};
use support::{TestEnv, TestRepo, same_path};

fn commit(repo: &TestRepo, file: &str, msg: &str) -> gix::ObjectId {
    repo.write(file, msg);
    repo.commit_all(msg)
}

fn id(text: &str) -> gix::ObjectId {
    gix::ObjectId::from_hex(text.as_bytes()).expect("hex id")
}

fn path_str(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn lists_local_with_upstream_and_gone() {
    let env = TestEnv::new();
    let remote = env.bare_remote("origin");
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    fixture.git(&["remote", "add", "origin", &path_str(&remote)]);
    fixture.git(&["push", "-q", "-u", "origin", "main"]);
    fixture.git(&["branch", "feat"]);
    fixture.git(&["push", "-q", "-u", "origin", "feat"]);
    fixture.git(&["branch", "plain"]);
    fixture.git(&["update-ref", "-d", "refs/remotes/origin/feat"]);

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let names: Vec<_> = list.local.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["feat", "main", "plain"], "sorted by short name");

    let main = &list.local[1];
    assert!(main.is_head, "main is checked out");
    let up = main.upstream.as_ref().expect("main has an upstream");
    assert_eq!(up.remote, "origin", "upstream remote");
    assert_eq!(up.merge.as_bstr(), "refs/heads/main", "upstream merge ref");
    assert_eq!(
        up.tracking.as_ref().map(|t| t.as_bstr().to_string()),
        Some("refs/remotes/origin/main".to_owned()),
        "tracking ref"
    );
    assert!(!up.gone, "tracking ref exists");

    let feat = list.local[0].upstream.as_ref().expect("feat is configured");
    assert!(
        feat.gone,
        "tracking ref of feat was deleted: configured but gone"
    );
    assert!(list.local[2].upstream.is_none(), "plain has no upstream");
    assert!(
        list.local[2].ahead_behind.is_none(),
        "ahead/behind is filled lazily"
    );
}

#[test]
fn remote_grouping_by_refspec_not_prefix() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["remote", "add", "foo", "https://example.com/foo.git"]);
    fixture.git(&["remote", "add", "foobar", "https://example.com/foobar.git"]);
    fixture.git(&["remote", "add", "origin", "https://example.com/o.git"]);
    fixture.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/custom/*",
    ]);
    for name in [
        "refs/remotes/foo/a",
        "refs/remotes/foobar/b",
        "refs/remotes/custom/x",
    ] {
        fixture.git(&["update-ref", name, &head.to_string()]);
    }

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let branches = |remote: &str| -> Vec<String> {
        list.remotes
            .iter()
            .find(|r| r.name == remote)
            .unwrap_or_else(|| panic!("remote {remote} listed"))
            .branches
            .iter()
            .map(|b| b.name.clone())
            .collect()
    };
    assert_eq!(branches("foo"), ["a"], "foo does not swallow foobar's refs");
    assert_eq!(branches("foobar"), ["b"], "foobar has its own branch");
    assert_eq!(
        branches("origin"),
        ["x"],
        "the fetch refspec decides, not the name"
    );
    let foo = list.remotes.iter().find(|r| r.name == "foo").expect("foo");
    assert_eq!(
        foo.fetch_url.as_deref(),
        Some("https://example.com/foo.git"),
        "fetch url"
    );
    assert_eq!(
        foo.push_url.as_deref(),
        Some("https://example.com/foo.git"),
        "push url falls back to url"
    );
}

#[test]
fn remote_head_symref_reported() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["remote", "add", "origin", "https://example.com/o.git"]);
    fixture.git(&["update-ref", "refs/remotes/origin/main", &head.to_string()]);
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let origin = &list.remotes[0];
    assert_eq!(
        origin.head_branch.as_deref(),
        Some("main"),
        "default branch from the symref"
    );
    let names: Vec<_> = origin.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main"], "refs/remotes/origin/HEAD is not a branch");
}

#[test]
fn annotated_and_lightweight_tags_peeled() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["tag", "light"]);
    fixture.git(&["tag", "-a", "ann", "-m", "first line\n\nbody"]);
    fixture.git(&["tag", "treetag", "HEAD^{tree}"]);
    fixture.git(&["tag", "-a", "annotated-tree", "-m", "a tree", "HEAD^{tree}"]);
    let tag_object = id(&fixture.git(&["rev-parse", "refs/tags/ann"]));
    let tree = id(&fixture.git(&["rev-parse", "HEAD^{tree}"]));

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let tag = |name: &str| {
        list.tags
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("tag {name} listed"))
    };
    assert_eq!(
        tag("light").target,
        head,
        "lightweight tag points at the commit"
    );
    assert!(tag("light").tag_object.is_none(), "no tag object");
    assert_eq!(
        tag("ann").target,
        head,
        "annotated tag is peeled to the commit"
    );
    assert_eq!(tag("ann").tag_object, Some(tag_object), "tag object id");
    assert_eq!(
        tag("ann").message_summary.as_deref(),
        Some("first line"),
        "summary"
    );
    assert!(tag("ann").tagger_time.is_some(), "tagger time");
    assert_eq!(
        tag("treetag").target,
        tree,
        "tree tags are listed with the tree id"
    );
    for (name, kind) in [
        ("light", gix::object::Kind::Commit),
        ("ann", gix::object::Kind::Commit),
        ("treetag", gix::object::Kind::Tree),
        ("annotated-tree", gix::object::Kind::Tree),
    ] {
        assert_eq!(
            tag(name).peeled_kind,
            kind,
            "{name}: the kind of the peeled object is reported"
        );
    }
    assert_eq!(
        tag("annotated-tree").target,
        tree,
        "an annotated tag of a tree is peeled to the tree"
    );

    let labels = list
        .decorations
        .0
        .get(&head)
        .expect("head commit is decorated");
    assert!(
        labels.contains(&RefLabel::Tag("ann".into())),
        "annotated tag decorates its commit"
    );
    assert!(
        labels.contains(&RefLabel::Tag("light".into())),
        "lightweight tag decorates"
    );
    assert!(
        !list.decorations.0.contains_key(&tree),
        "tags of trees are not decorated"
    );
    assert!(
        !refs::decorations(&list).0.contains_key(&tree),
        "the pure decorations() agrees with list(): the kind is part of TagInfo"
    );
}

#[test]
fn version_sort() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    for name in ["v1.10", "v1.9", "v1.2", "v10.0", "v1.9.1", "beta"] {
        fixture.git(&["tag", name]);
    }
    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let names: Vec<_> = list.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["beta", "v1.2", "v1.9", "v1.9.1", "v1.10", "v10.0"],
        "digit runs compare numerically"
    );
}

#[test]
fn stash_list_order_and_branch() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let base = commit(&fixture, "a.txt", "one");
    fixture.write("a.txt", "two");
    fixture.git(&["stash", "push", "-q", "-m", "first"]);
    fixture.write("a.txt", "three");
    fixture.write("new.txt", "untracked");
    fixture.git(&["stash", "push", "-q", "-u", "-m", "second"]);
    fixture.git(&["checkout", "-q", "--detach"]);
    fixture.write("a.txt", "four");
    fixture.git(&["stash", "push", "-q"]);

    let repo = fixture.open();
    let stashes = refs::stash_list(&repo).expect("stash list");
    assert_eq!(stashes.len(), 3, "three entries");
    assert_eq!(
        stashes.iter().map(|s| s.index).collect::<Vec<_>>(),
        [0, 1, 2],
        "stash@{{0}} first"
    );
    assert!(
        stashes[0].message.starts_with("WIP on (no branch)"),
        "{}",
        stashes[0].message
    );
    assert_eq!(stashes[0].branch, None, "(no branch) is None");
    assert_eq!(
        stashes[1].branch.as_deref(),
        Some("main"),
        "branch of `On main: second`"
    );
    assert!(
        stashes[1].message.starts_with("On main: second"),
        "{}",
        stashes[1].message
    );
    assert!(
        stashes[1].untracked_commit.is_some(),
        "-u stash has a third parent"
    );
    assert!(stashes[1].index_commit.is_some(), "index commit");
    assert_eq!(stashes[2].base, base, "base commit");
    assert_eq!(stashes[2].branch.as_deref(), Some("main"), "WIP on main");
    assert!(stashes[2].untracked_commit.is_none(), "no untracked commit");
    let first = id(&fixture.git(&["rev-parse", "stash@{2}"]));
    assert_eq!(stashes[2].id, first, "oldest stash is stash@{{2}}");

    let list = refs::list(&repo, RefListOptions::ALL).expect("refs list");
    assert_eq!(list.stashes.len(), 3, "list includes stashes");
    assert!(
        list.decorations.0[&stashes[0].id].contains(&RefLabel::Stash(0)),
        "stash commits are decorated"
    );
    let none = refs::list(&repo, RefListOptions::default()).expect("refs list");
    assert!(
        none.stashes.is_empty() && none.tags.is_empty() && none.remotes.is_empty(),
        "opts off"
    );
}

#[test]
fn decorations_map() {
    let env = TestEnv::new();
    let remote = env.bare_remote("origin");
    let fixture = env.repo("repo");
    let first = commit(&fixture, "a.txt", "one");
    fixture.git(&["remote", "add", "origin", &path_str(&remote)]);
    fixture.git(&["push", "-q", "origin", "main"]);
    fixture.git(&["fetch", "-q", "origin"]);
    fixture.git(&["tag", "t1"]);
    let second = commit(&fixture, "a.txt", "two");
    fixture.git(&["branch", "side"]);

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let at_first = &list.decorations.0[&first];
    assert!(at_first.contains(&RefLabel::Tag("t1".into())), "tag label");
    assert!(
        at_first.contains(&RefLabel::RemoteBranch {
            remote: "origin".into(),
            name: "main".into()
        }),
        "remote branch label"
    );
    let at_second = &list.decorations.0[&second];
    assert!(at_second.contains(&RefLabel::Head), "HEAD label");
    assert!(
        at_second.contains(&RefLabel::LocalBranch("main".into())),
        "main label"
    );
    assert!(
        at_second.contains(&RefLabel::LocalBranch("side".into())),
        "side label"
    );

    let recomputed = refs::decorations(&list);
    assert_eq!(
        recomputed.0.get(&second).map(|l| l.len()),
        Some(3),
        "the pure function agrees with list()"
    );
}

#[test]
fn ahead_behind_counts_and_cap() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "base");
    fixture.git(&["checkout", "-q", "-b", "topic"]);
    for i in 0..3 {
        commit(&fixture, "t.txt", &format!("t{i}"));
    }
    fixture.git(&["checkout", "-q", "main"]);
    for i in 0..2 {
        commit(&fixture, "m.txt", &format!("m{i}"));
    }
    let topic = id(&fixture.git(&["rev-parse", "topic"]));
    let main = id(&fixture.git(&["rev-parse", "main"]));
    let repo = fixture.open();
    let cancel = CancelToken::new();

    let out = refs::ahead_behind(&repo, &[(topic, main), (main, main)], 999, &cancel)
        .expect("ahead/behind");
    assert_eq!(
        (out[0].ahead, out[0].behind, out[0].capped),
        (3, 2, false),
        "topic is 3 ahead of and 2 behind main"
    );
    assert_eq!((out[1].ahead, out[1].behind), (0, 0), "equal ids");
    assert_eq!(
        fixture.git(&["rev-list", "--count", "main..topic"]),
        "3",
        "oracle agrees"
    );

    let capped = refs::ahead_behind(&repo, &[(topic, main)], 2, &cancel).expect("capped");
    assert_eq!(capped[0].ahead, 2, "ahead stops at the cap");
    assert!(capped[0].capped, "the cap is reported");

    cancel.cancel();
    let err = refs::ahead_behind(&repo, &[(topic, main)], 999, &cancel).expect_err("cancelled");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "cancel is honoured");
}

#[test]
fn checked_out_in_other_worktree() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    commit(&fixture, "a.txt", "one");
    fixture.git(&["branch", "feat"]);
    let wt = env.path("wt");
    fixture.git(&["worktree", "add", "-q", &path_str(&wt), "feat"]);

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let feat = list.local.iter().find(|b| b.name == "feat").expect("feat");
    let other = feat
        .checked_out_in
        .as_ref()
        .expect("feat is checked out elsewhere");
    assert!(same_path(other, &wt), "worktree path: {}", other.display());
    let main = list.local.iter().find(|b| b.name == "main").expect("main");
    assert!(
        main.checked_out_in.is_none(),
        "the current worktree is excluded"
    );

    let from_linked = refs::list(
        &Repo::open(&env.rt, &wt).expect("linked worktree opens"),
        RefListOptions::ALL,
    )
    .expect("list");
    let main = from_linked
        .local
        .iter()
        .find(|b| b.name == "main")
        .expect("main");
    assert!(
        main.checked_out_in
            .as_ref()
            .is_some_and(|p| same_path(p, &fixture.path)),
        "seen from the linked worktree, main is checked out in the main one"
    );
}

#[test]
fn detached_head_label() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let first = commit(&fixture, "a.txt", "one");
    commit(&fixture, "a.txt", "two");
    fixture.git(&["checkout", "-q", "--detach", &first.to_string()]);

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    assert_eq!(
        list.head,
        HeadState::Detached { target: first },
        "detached head state"
    );
    assert!(
        list.local.iter().all(|b| !b.is_head),
        "no branch is current"
    );
    assert!(
        list.decorations.0[&first].contains(&RefLabel::Head),
        "HEAD decorates the detached commit"
    );
}

#[test]
fn packed_refs_only_repo() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["branch", "other"]);
    fixture.git(&["tag", "-a", "v1", "-m", "release"]);
    fixture.git(&["pack-refs", "--all", "--prune"]);
    assert!(
        !fixture.path.join(".git/refs/heads/other").exists(),
        "the fixture has no loose refs"
    );

    let list = refs::list(&fixture.open(), RefListOptions::ALL).expect("refs list");
    let names: Vec<_> = list.local.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main", "other"], "packed branches");
    assert_eq!(list.tags[0].target, head, "packed annotated tag peeled");
}

#[test]
fn is_merged_true_false() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let base = commit(&fixture, "a.txt", "base");
    fixture.git(&["checkout", "-q", "-b", "topic"]);
    let topic = commit(&fixture, "t.txt", "topic");
    fixture.git(&["checkout", "-q", "main"]);
    let main = commit(&fixture, "m.txt", "main");
    let repo = fixture.open();

    assert!(
        refs::is_merged(&repo, base, None).expect("merged"),
        "base is an ancestor of HEAD"
    );
    assert!(
        refs::is_merged(&repo, main, None).expect("merged"),
        "HEAD itself"
    );
    assert!(
        !refs::is_merged(&repo, topic, None).expect("merged"),
        "diverged branch"
    );
    assert!(
        refs::is_merged(&repo, base, Some(topic)).expect("merged"),
        "explicit target"
    );
    assert!(
        !refs::is_merged(&repo, main, Some(topic)).expect("merged"),
        "not in topic"
    );

    fixture.git(&["checkout", "-q", "--orphan", "island"]);
    let island = commit(&fixture, "i.txt", "unrelated");
    let repo = fixture.open();
    assert!(
        !refs::is_merged(&repo, main, Some(island)).expect("no merge base is not an error"),
        "unrelated histories"
    );
}

#[test]
fn is_reachable_from_any_of_several_tips() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let base = commit(&fixture, "a.txt", "base");
    fixture.git(&["checkout", "-q", "-b", "topic"]);
    let topic = commit(&fixture, "t.txt", "topic");
    let topic2 = commit(&fixture, "t.txt", "topic 2");
    fixture.git(&["checkout", "-q", "main"]);
    let main = commit(&fixture, "m.txt", "main");
    fixture.git(&["checkout", "-q", "--orphan", "island"]);
    let island = commit(&fixture, "i.txt", "unrelated");
    let repo = fixture.open();

    let reachable = |id, tips: &[_]| refs::is_reachable_from(&repo, id, tips).expect("walk");
    assert!(reachable(main, &[island, main]), "a tip itself");
    assert!(reachable(base, &[island, main]), "an ancestor of one tip");
    assert!(
        reachable(topic, &[main, topic2]),
        "an ancestor of the second tip"
    );
    assert!(!reachable(topic2, &[main, island, base]), "on no tip");
    assert!(!reachable(main, &[]), "no tips");
}

#[test]
fn resolve_commit_peels_and_reports_missing() {
    let env = TestEnv::new();
    let fixture = env.repo("repo");
    let head = commit(&fixture, "a.txt", "one");
    fixture.git(&["tag", "-a", "v1", "-m", "release"]);
    let repo = fixture.open();
    assert_eq!(
        refs::resolve_commit(&repo, "v1").expect("tag"),
        head,
        "annotated tag peels"
    );
    assert_eq!(
        refs::resolve_commit(&repo, "HEAD~0").expect("head"),
        head,
        "revision syntax"
    );
    for spec in ["nope", "HEAD^{tree}"] {
        let err = refs::resolve_commit(&repo, spec).expect_err("not a commit");
        assert_eq!(err.kind(), GitErrorKind::NotFound, "{spec}");
    }
}
