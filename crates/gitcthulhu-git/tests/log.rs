//! Log walking, search and message reads (WP-B4, spec §2.5 `log.rs`).

#[path = "support/graphs.rs"]
mod graphs;
mod support;

use std::{collections::HashMap, time::Duration};

use gitcthulhu_git::{
    CancelToken, CommitInfo, GitErrorKind, GraphBuilder, LogOptions, LogOrder, LogTips, LogWalker,
    NodeKind, Repo, SearchQuery,
};
use gix::ObjectId;
use graphs::Dag;
use support::{TestEnv, TestRepo};

// ───────────────────────────── helpers ─────────────────────────────

fn walk_all(repo: &Repo, opts: &LogOptions) -> Vec<CommitInfo> {
    let cancel = CancelToken::new();
    let mut walker = LogWalker::new(repo, opts, &cancel).expect("the walk starts");
    let mut commits = Vec::new();
    loop {
        let page = walker.next_page(1000, &cancel).expect("a page loads");
        commits.extend(page.commits);
        if page.exhausted {
            break;
        }
    }
    commits
}

fn subjects(commits: &[CommitInfo]) -> Vec<String> {
    commits.iter().map(|c| c.summary.to_string()).collect()
}

fn ids(commits: &[CommitInfo]) -> Vec<ObjectId> {
    commits.iter().map(|c| c.id).collect()
}

/// The ids printed one per line by a git command.
fn parse_ids(text: &str) -> Vec<ObjectId> {
    text.lines()
        .map(|line| ObjectId::from_hex(line.trim().as_bytes()).expect("git prints hex ids"))
        .collect()
}

fn rev_parse(repo: &TestRepo, rev: &str) -> ObjectId {
    ObjectId::from_hex(repo.git(&["rev-parse", rev]).as_bytes()).expect("git prints a hex id")
}

fn branches_only() -> LogOptions {
    LogOptions {
        tips: LogTips::AllRefs {
            remotes: false,
            tags: false,
            stashes: false,
        },
        ..LogOptions::default()
    }
}

fn imported(env: &TestEnv, name: &str, dag: &Dag) -> TestRepo {
    let repo = env.repo(name);
    repo.fast_import(&dag.fast_import_stream());
    repo
}

/// Two branches whose commits alternate in time: `a1 b1 a2 b2 a3 b3`, on top of `base`.
fn interleaved() -> Dag {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let a1 = dag.add("a1", &[base]);
    let b1 = dag.add("b1", &[base]);
    let a2 = dag.add("a2", &[a1]);
    let b2 = dag.add("b2", &[b1]);
    let a3 = dag.add("a3", &[a2]);
    let b3 = dag.add("b3", &[b2]);
    dag.set_ref("refs/heads/main", a3);
    dag.set_ref("refs/heads/b", b3);
    dag
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        in_message: false,
        in_author: false,
        in_committer: false,
        in_hash: false,
        case_sensitive: false,
    }
}

/// A `file://` URL for a local path (needed for shallow clones; plain paths ignore `--depth`).
fn file_url(path: &std::path::Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

// ───────────────────────────── order ─────────────────────────────

#[test]
fn walk_order_children_before_parents() {
    let env = TestEnv::new();
    let dag = graphs::branchy(2000, 8, 3);
    let repo = imported(&env, "branchy", &dag);
    let commits = walk_all(&repo.open(), &LogOptions::default());
    assert_eq!(commits.len(), dag.len(), "every commit is walked once");
    let position: HashMap<ObjectId, usize> =
        commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    for (idx, commit) in commits.iter().enumerate() {
        for parent in &commit.parents {
            assert!(
                position[parent] > idx,
                "parent {parent} of {} comes after its child",
                commit.summary
            );
        }
    }
    // The generator's emission order has strictly increasing times, so date order is its exact reverse.
    let expected: Vec<String> = dag
        .walk_order()
        .map(|n| dag.nodes[n].name.clone())
        .collect();
    assert_eq!(
        subjects(&commits),
        expected,
        "date order sorts independent lines by committer time"
    );
}

#[test]
fn date_order_interleaves_branches() {
    let env = TestEnv::new();
    let repo = imported(&env, "interleaved", &interleaved());
    let commits = walk_all(&repo.open(), &branches_only());
    // Branches are interleaved chronologically, like `git rev-list --date-order`.
    snapbox::assert_data_eq!(
        subjects(&commits).join(" "),
        snapbox::str!["b3 a3 b2 a2 b1 a1 base"]
    );
    let oracle = parse_ids(&repo.git(&["rev-list", "--date-order", "HEAD", "--branches"]));
    assert_eq!(ids(&commits), oracle, "the walk equals git's date order");
}

#[test]
fn topo_order_groups_lines() {
    let env = TestEnv::new();
    let repo = imported(&env, "interleaved", &interleaved());
    let opts = LogOptions {
        order: LogOrder::Topo,
        ..branches_only()
    };
    let commits = walk_all(&repo.open(), &opts);
    // Each line of history is kept together instead of being interleaved by time.
    snapbox::assert_data_eq!(
        subjects(&commits).join(" "),
        snapbox::str!["b3 b2 b1 a3 a2 a1 base"]
    );
    let oracle = parse_ids(&repo.git(&["rev-list", "--topo-order", "HEAD", "--branches"]));
    assert_eq!(ids(&commits), oracle, "the walk equals git's topo order");
}

// ───────────────────────────── tips ─────────────────────────────

/// A repository with commits reachable only from a remote-tracking branch, an annotated tag and a stash, plus a tag
/// of a tree.
fn repo_with_all_kinds_of_tips(env: &TestEnv) -> TestRepo {
    let repo = env.repo("tips");
    repo.write("file.txt", "base\n");
    repo.commit_all("base");

    repo.git(&["checkout", "-q", "-b", "r"]);
    repo.commit_all("remote only");
    repo.git(&["update-ref", "refs/remotes/origin/r", "HEAD"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["branch", "-q", "-D", "r"]);

    repo.git(&["checkout", "-q", "--detach"]);
    repo.commit_all("tagged only");
    repo.git(&["tag", "-a", "v1", "-m", "release"]);
    repo.git(&["checkout", "-q", "main"]);
    repo.git(&["tag", "tree-tag", "HEAD^{tree}"]);

    repo.write("file.txt", "stashed\n");
    repo.git(&["stash", "push", "-q", "-m", "stashed"]);
    repo
}

#[test]
fn all_refs_tips_include_remote_and_tags_and_stash() {
    let env = TestEnv::new();
    let repo = repo_with_all_kinds_of_tips(&env);
    let backend = repo.open();

    let mut all = subjects(&walk_all(&backend, &LogOptions::default()));
    all.sort();
    // Remote-tracking branches, annotated tags (peeled) and stashes are tips; the tree tag is skipped.
    snapbox::assert_data_eq!(
        all.join("\n"),
        snapbox::str![[r#"
On main: stashed
base
remote only
tagged only
"#]]
    );

    let cancel = CancelToken::new();
    let walker =
        LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("the walk starts");
    assert_eq!(
        walker.stash_ids().iter().copied().collect::<Vec<_>>(),
        [rev_parse(&repo, "refs/stash")],
        "the stash is known to the walker"
    );

    let only_branches = subjects(&walk_all(&backend, &branches_only()));
    assert_eq!(
        only_branches,
        ["base"],
        "without remotes, tags and stashes only branch history remains"
    );

    let tags_only = subjects(&walk_all(
        &backend,
        &LogOptions {
            tips: LogTips::AllRefs {
                remotes: false,
                tags: true,
                stashes: false,
            },
            ..LogOptions::default()
        },
    ));
    assert_eq!(tags_only, ["tagged only", "base"], "tags add their commits");
}

#[test]
fn explicit_tips_walk_exactly_those() {
    let env = TestEnv::new();
    let repo = repo_with_all_kinds_of_tips(&env);
    let backend = repo.open();
    let remote = rev_parse(&repo, "refs/remotes/origin/r");
    let opts = LogOptions {
        tips: LogTips::Explicit(vec![remote, remote]),
        ..LogOptions::default()
    };
    assert_eq!(
        subjects(&walk_all(&backend, &opts)),
        ["remote only", "base"],
        "an explicit tip walks its own history only, once"
    );

    let tree = rev_parse(&repo, "HEAD^{tree}");
    let err = LogWalker::new(
        &backend,
        &LogOptions {
            tips: LogTips::Explicit(vec![tree]),
            ..LogOptions::default()
        },
        &CancelToken::new(),
    )
    .err()
    .expect("a tree is no commit");
    assert_eq!(
        err.kind(),
        GitErrorKind::NotFound,
        "an explicit tip must be a commit"
    );
}

#[test]
fn stash_helpers_hidden() {
    let env = TestEnv::new();
    let repo = env.repo("stash");
    repo.write("file.txt", "base\n");
    repo.commit_all("base");
    repo.write("file.txt", "staged\n");
    repo.git(&["add", "file.txt"]);
    repo.write("untracked.txt", "new\n");
    repo.git(&["stash", "push", "-q", "-u", "-m", "with untracked"]);
    let stash = rev_parse(&repo, "refs/stash");
    let index_commit = rev_parse(&repo, "refs/stash^2");
    let untracked_commit = rev_parse(&repo, "refs/stash^3");

    let commits = walk_all(&repo.open(), &LogOptions::default());
    assert_eq!(
        subjects(&commits),
        ["On main: with untracked", "base"],
        "the stash's index and untracked commits are never emitted"
    );
    let walked = ids(&commits);
    assert!(
        !walked.contains(&index_commit) && !walked.contains(&untracked_commit),
        "helper commits are hidden"
    );
    assert_eq!(commits[0].id, stash, "the stash itself is shown");
    assert_eq!(
        commits[0].parents.len(),
        3,
        "the stash keeps its real parents in CommitInfo"
    );
}

#[test]
fn oplog_refs_never_walked() {
    let env = TestEnv::new();
    let repo = env.repo("oplog");
    repo.write("file.txt", "base\n");
    repo.commit_all("base");
    let unrelated = repo.git(&["commit-tree", "HEAD^{tree}", "-m", "oplog snapshot"]);
    repo.git(&["update-ref", "refs/gitcthulhu/oplog/main", &unrelated]);
    repo.git(&["update-ref", "refs/gitcthulhu/redo/x", &unrelated]);
    let commits = walk_all(&repo.open(), &LogOptions::default());
    assert_eq!(
        subjects(&commits),
        ["base"],
        "refs/gitcthulhu/* are not tips"
    );
}

// ───────────────────────────── pages ─────────────────────────────

#[test]
fn pagination_concatenates_to_full_walk() {
    let env = TestEnv::new();
    let dag = graphs::branchy(1500, 6, 11);
    let repo = imported(&env, "pages", &dag);
    let backend = repo.open();
    let full = ids(&walk_all(&backend, &LogOptions::default()));

    let cancel = CancelToken::new();
    let mut walker =
        LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("the walk starts");
    let mut paged = Vec::new();
    for n in [7, 0, 100, 1].into_iter().chain(std::iter::repeat(333)) {
        let page = walker.next_page(n, &cancel).expect("a page loads");
        assert!(
            page.commits.len() <= n,
            "a page has at most the requested size"
        );
        paged.extend(ids(&page.commits));
        assert_eq!(
            walker.loaded(),
            paged.len(),
            "loaded() counts the returned commits"
        );
        if page.exhausted {
            break;
        }
        assert_eq!(page.commits.len(), n, "only the last page may be short");
    }
    assert_eq!(paged, full, "pages concatenate to the full walk");
    assert!(walker.is_exhausted(), "the walk is exhausted");
    let after = walker
        .next_page(10, &cancel)
        .expect("an exhausted walk still answers");
    assert!(
        after.commits.is_empty() && after.exhausted,
        "nothing follows the last page"
    );

    let mut exact =
        LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("the walk starts");
    let page = exact.next_page(dag.len(), &cancel).expect("a page loads");
    assert!(
        page.exhausted,
        "a page ending exactly at the last commit reports exhaustion"
    );
}

#[test]
fn first_parent_only() {
    let env = TestEnv::new();
    let repo = imported(&env, "merges", &graphs::mainline_with_merges(5));
    let opts = LogOptions {
        first_parent: true,
        ..branches_only()
    };
    let backend = repo.open();
    let cancel = CancelToken::new();
    let walker = LogWalker::new(&backend, &opts, &cancel).expect("the walk starts");
    assert!(
        walker.first_parent(),
        "the walker reports its mode for GraphBuilder::new"
    );
    let commits = walk_all(&backend, &opts);
    let oracle = parse_ids(&repo.git(&["rev-list", "--first-parent", "HEAD"]));
    assert_eq!(
        ids(&commits),
        oracle,
        "only first parents are followed, like git --first-parent"
    );
    assert!(
        commits.iter().all(|c| !c.summary.starts_with("side")),
        "side branches are never walked"
    );
    assert_eq!(
        commits[0].parents.len(),
        2,
        "merges still list all their parents"
    );
}

// ───────────────────────────── commit data ─────────────────────────────

#[test]
fn mailmap_applied() {
    let env = TestEnv::new();
    let repo = env.repo("mailmap");
    repo.write("file.txt", "x\n");
    repo.git(&["add", "file.txt"]);
    repo.git(&[
        "-c",
        "user.name=Old Name",
        "-c",
        "user.email=old@example.com",
        "commit",
        "-q",
        "-m",
        "first",
    ]);
    repo.write(".mailmap", "New Name <new@example.com> <old@example.com>\n");
    let backend = repo.open();

    let mapped = walk_all(&backend, &LogOptions::default());
    let person = &mapped[0].author.person;
    assert_eq!(
        (person.name.to_string(), person.email.to_string()),
        ("New Name".into(), "new@example.com".into()),
        "the mailmap canonicalises the author"
    );
    assert_eq!(
        mapped[0].committer.person.name, "New Name",
        "and the committer"
    );

    let raw = walk_all(
        &backend,
        &LogOptions {
            mailmap: false,
            ..LogOptions::default()
        },
    );
    assert_eq!(
        raw[0].author.person.name, "Old Name",
        "without the mailmap the raw name is kept"
    );
}

#[test]
fn non_utf8_encoding_header_decoded() {
    let env = TestEnv::new();
    let repo = env.repo("latin1");
    repo.write("file.txt", "x\n");
    repo.git(&["add", "file.txt"]);
    repo.write("msg.txt", b"Gr\xfc\xdfe\n\nK\xf6rper\n");
    repo.git(&[
        "-c",
        "i18n.commitEncoding=ISO-8859-1",
        "commit",
        "-q",
        "-F",
        "msg.txt",
    ]);
    let backend = repo.open();
    let commits = walk_all(&backend, &branches_only());
    assert_eq!(
        &*commits[0].summary, "Grüße",
        "the summary is decoded with the encoding header"
    );
    assert!(commits[0].has_body, "the message has a body");
    let messages = gitcthulhu_git::log::messages(&backend, &[commits[0].id], &CancelToken::new())
        .expect("messages load");
    assert_eq!(
        &*messages[0], "Grüße\n\nKörper",
        "full messages are decoded too"
    );
}

#[test]
fn shallow_boundary_flag() {
    let env = TestEnv::new();
    let source = env.repo("source");
    for n in 1..=5 {
        source.write("file.txt", format!("{n}\n"));
        source.commit_all(&format!("c{n}"));
    }
    let clone_path = env.path("shallow");
    env.git_global(&[
        "clone",
        "-q",
        "--depth",
        "2",
        &file_url(&source.path),
        &clone_path.to_string_lossy(),
    ]);
    let backend = Repo::open(&env.rt, &clone_path).expect("the clone opens");
    let cancel = CancelToken::new();
    let mut walker =
        LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("the walk starts");
    let page = walker
        .next_page(100, &cancel)
        .expect("the walk does not need the missing parents");
    assert_eq!(
        subjects(&page.commits),
        ["c5", "c4"],
        "only the fetched depth is walked"
    );
    assert!(page.exhausted, "the walk ends at the boundary");
    assert!(
        walker.is_shallow_boundary(&page.commits[1].id)
            && !walker.is_shallow_boundary(&page.commits[0].id),
        "exactly the oldest fetched commit is a shallow boundary"
    );
    assert_eq!(
        page.commits[1].parents.len(),
        1,
        "CommitInfo keeps the real (missing) parent"
    );

    let mut builder = GraphBuilder::new(walker.first_parent());
    builder.push_page(&page, &walker);
    let boundary = builder.layout().row(1);
    assert_eq!(
        boundary.kind,
        NodeKind::ShallowBoundary,
        "the graph marks the boundary"
    );
    assert!(!boundary.line_down, "no line leads to the missing parent");
    assert_eq!(
        builder.open_lanes(),
        0,
        "no lane waits for the missing parent"
    );
}

#[test]
fn commit_info_and_head_message() {
    let env = TestEnv::new();
    let repo = env.repo("info");
    assert_eq!(
        gitcthulhu_git::log::head_message(&repo.open()).expect("unborn HEAD is fine"),
        None,
        "an unborn HEAD has no message"
    );
    repo.write("file.txt", "1\n");
    let first = repo.commit_all("first");
    repo.write("file.txt", "2\n");
    repo.git(&[
        "commit",
        "-q",
        "-a",
        "-m",
        "  second subject  ",
        "-m",
        "body line",
    ]);
    let second = rev_parse(&repo, "HEAD");
    let backend = repo.open();

    assert_eq!(
        gitcthulhu_git::log::head_message(&backend).expect("HEAD has a message"),
        Some("  second subject\n\nbody line".into()),
        "the full message of HEAD prefills an amend, as stored (only trailing whitespace is dropped)"
    );
    let info = gitcthulhu_git::log::commit_info(&backend, second).expect("the commit exists");
    assert_eq!(
        &*info.summary, "second subject",
        "the summary is the first line, trimmed"
    );
    assert!(info.has_body, "a body follows");
    assert_eq!(info.parents.as_slice(), [first], "parents are listed");
    assert_eq!(
        info.author.person.name, "Test User",
        "the author comes from the commit"
    );
    assert_eq!(
        info.committer.time.offset_minutes, 0,
        "the fixture dates are UTC"
    );

    let missing = ObjectId::from_hex(b"1234567890123456789012345678901234567890").expect("hex");
    let err = gitcthulhu_git::log::commit_info(&backend, missing).expect_err("unknown commit");
    assert_eq!(
        err.kind(),
        GitErrorKind::NotFound,
        "an unknown id is NotFound"
    );
}

#[test]
fn commit_messages_oldest_first() {
    let env = TestEnv::new();
    let repo = env.repo("range");
    let base = repo.commit_all("base");
    for n in 1..=4 {
        repo.git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("change {n}"),
            "-m",
            &format!("body {n}"),
        ]);
    }
    let head = rev_parse(&repo, "HEAD");
    let backend = repo.open();
    let all = gitcthulhu_git::log::commit_messages(&backend, Some(base), head, 10)
        .expect("the range is walked");
    assert_eq!(
        all,
        [
            "change 1\n\nbody 1",
            "change 2\n\nbody 2",
            "change 3\n\nbody 3",
            "change 4\n\nbody 4"
        ],
        "base..head, oldest first, full messages"
    );
    let limited = gitcthulhu_git::log::commit_messages(&backend, Some(base), head, 2)
        .expect("the range is walked");
    assert_eq!(
        limited,
        ["change 3\n\nbody 3", "change 4\n\nbody 4"],
        "the limit keeps the newest commits"
    );
    let everything = gitcthulhu_git::log::commit_messages(&backend, None, head, 100)
        .expect("the history is walked");
    assert_eq!(
        everything.len(),
        5,
        "without a base the whole history is included"
    );
}

#[test]
fn messages_returns_full_bodies_in_order() {
    let env = TestEnv::new();
    let repo = env.repo("messages");
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "one",
        "-m",
        "first body",
    ]);
    let one = rev_parse(&repo, "HEAD");
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "two",
        "-m",
        "second body\nwith two lines",
    ]);
    let two = rev_parse(&repo, "HEAD");
    let unknown = ObjectId::from_hex(b"1234567890123456789012345678901234567890").expect("hex");
    let tree = rev_parse(&repo, "HEAD^{tree}");
    let backend = repo.open();
    let messages =
        gitcthulhu_git::log::messages(&backend, &[two, unknown, one, tree], &CancelToken::new())
            .expect("messages load");
    let messages: Vec<&str> = messages.iter().map(|m| &**m).collect();
    assert_eq!(
        messages,
        [
            "two\n\nsecond body\nwith two lines",
            "",
            "one\n\nfirst body",
            ""
        ],
        "full messages in the given order; unknown ids and non-commits give empty strings"
    );

    let cancel = CancelToken::new();
    cancel.cancel();
    let err = gitcthulhu_git::log::messages(&backend, &[one], &cancel).expect_err("cancelled");
    assert_eq!(err.kind(), GitErrorKind::Cancelled, "cancel is honoured");
}

// ───────────────────────────── search ─────────────────────────────

#[test]
fn search_hash_prefix_and_author() {
    let env = TestEnv::new();
    let repo = env.repo("search");
    for (n, (name, email)) in [
        ("Alice Example", "alice@example.com"),
        ("Bob", "bob@example.com"),
    ]
    .iter()
    .cycle()
    .take(6)
    .enumerate()
    {
        repo.git(&[
            "-c",
            &format!("user.name={name}"),
            "-c",
            &format!("user.email={email}"),
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("change {n}"),
            "-m",
            if n == 3 {
                "Fixes the FROBNICATOR"
            } else {
                "plain"
            },
        ]);
    }
    let backend = repo.open();
    let tips = LogTips::AllRefs {
        remotes: true,
        tags: true,
        stashes: true,
    };
    let cancel = CancelToken::new();
    let run = |q: &SearchQuery, limit: usize| {
        let mut found = Vec::new();
        let count = gitcthulhu_git::log::search(&backend, q, &tips, limit, &cancel, &mut |c| {
            found.push(c.summary.to_string())
        })
        .expect("search runs");
        assert_eq!(count, found.len(), "the count equals the reported matches");
        found
    };

    let by_author = run(
        &SearchQuery {
            in_author: true,
            ..query("ALICE")
        },
        100,
    );
    assert_eq!(
        by_author,
        ["change 4", "change 2", "change 0"],
        "author search is case-insensitive, in walk order"
    );
    let case_sensitive = run(
        &SearchQuery {
            in_author: true,
            case_sensitive: true,
            ..query("ALICE")
        },
        100,
    );
    assert!(
        case_sensitive.is_empty(),
        "case-sensitive search respects case"
    );

    let target = rev_parse(&repo, "HEAD~2");
    let prefix = target.to_hex_with_len(8).to_string().to_uppercase();
    let by_hash = run(
        &SearchQuery {
            in_hash: true,
            ..query(&prefix)
        },
        100,
    );
    assert_eq!(
        by_hash,
        ["change 3"],
        "a hex prefix finds its commit, case-insensitively"
    );

    let in_body = run(
        &SearchQuery {
            in_message: true,
            ..query("frobnicator")
        },
        100,
    );
    assert_eq!(in_body, ["change 3"], "message search covers the body");

    let limited = run(
        &SearchQuery {
            in_message: true,
            ..query("change")
        },
        2,
    );
    assert_eq!(
        limited,
        ["change 5", "change 4"],
        "the search stops at the limit"
    );
    assert!(
        run(&query(""), 10).is_empty(),
        "an empty query matches nothing"
    );
}

// ───────────────────────────── cancellation and commit-graph ─────────────────────────────

#[test]
fn new_cancel_returns_promptly() {
    let env = TestEnv::new();
    let dag = graphs::branchy(30_000, 16, 5);
    let repo = imported(&env, "big", &dag);
    let backend = repo.open();

    let cancelled = CancelToken::new();
    cancelled.cancel();
    let started = std::time::Instant::now();
    let err = LogWalker::new(&backend, &LogOptions::default(), &cancelled)
        .err()
        .expect("a cancelled token stops the walk");
    assert_eq!(
        err.kind(),
        GitErrorKind::Cancelled,
        "cancel is reported as such"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a cancelled walk returns at once"
    );

    // Cancelled while the topological build (a full walk without a commit-graph) is running.
    let cancel = CancelToken::new();
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            cancel.cancel();
            std::time::Instant::now()
        })
    };
    let result = LogWalker::new(&backend, &LogOptions::default(), &cancel);
    let returned = std::time::Instant::now();
    let cancelled_at = canceller.join().expect("the canceller does not panic");
    match result {
        Err(err) => {
            assert_eq!(
                err.kind(),
                GitErrorKind::Cancelled,
                "cancel is reported as such"
            );
            assert!(
                returned.saturating_duration_since(cancelled_at) < Duration::from_millis(500),
                "the caller stops waiting as soon as it is cancelled"
            );
        }
        Ok(_) => println!(
            "the walk build finished before the cancel; the in-flight cancel was not exercised"
        ),
    }

    // A cancelled page loses nothing: the next page starts with the commits already read.
    let mut walker =
        LogWalker::new(&backend, &LogOptions::default(), &CancelToken::new()).expect("walk starts");
    let first = walker
        .next_page(10, &CancelToken::new())
        .expect("a page loads");
    let stop = CancelToken::new();
    stop.cancel();
    let err = walker.next_page(1000, &stop).expect_err("a cancelled page");
    assert_eq!(
        err.kind(),
        GitErrorKind::Cancelled,
        "next_page honours cancel"
    );
    let second = walker
        .next_page(10, &CancelToken::new())
        .expect("a page loads");
    let mut reference =
        LogWalker::new(&backend, &LogOptions::default(), &CancelToken::new()).expect("walk starts");
    let expected = reference
        .next_page(20, &CancelToken::new())
        .expect("a page loads");
    assert_eq!(
        [ids(&first.commits), ids(&second.commits)].concat(),
        ids(&expected.commits),
        "pagination continues seamlessly after a cancelled page"
    );
}

#[test]
fn commit_graph_used_when_present() {
    let env = TestEnv::new();
    let dag = graphs::branchy(3000, 10, 9);
    let repo = imported(&env, "graph", &dag);
    let backend = repo.open();
    let topo = LogOptions {
        order: LogOrder::Topo,
        ..LogOptions::default()
    };
    let date_before = ids(&walk_all(&backend, &LogOptions::default()));
    let topo_before = ids(&walk_all(&backend, &topo));

    repo.git(&["commit-graph", "write", "--reachable"]);
    assert!(
        repo.path.join(".git/objects/info/commit-graph").is_file()
            || repo.path.join(".git/objects/info/commit-graphs").is_dir(),
        "git wrote a commit-graph"
    );
    let date_after = ids(&walk_all(&backend, &LogOptions::default()));
    let topo_after = ids(&walk_all(&backend, &topo));
    assert_eq!(
        date_after, date_before,
        "the commit-graph does not change the date order"
    );
    assert_eq!(
        topo_after, topo_before,
        "the commit-graph does not change the topo order"
    );
    // Committer times are unique in the fixture, so git's date order is fully determined and serves as the oracle.
    let oracle = parse_ids(&repo.git(&[
        "rev-list",
        "--date-order",
        "HEAD",
        "--branches",
        "--remotes",
        "--tags",
    ]));
    assert_eq!(
        date_after, oracle,
        "the walk with a commit-graph equals git's date order"
    );

    // Commits newer than the commit-graph are walked too.
    let main = rev_parse(&repo, "main");
    let newer = repo.git(&[
        "commit-tree",
        "HEAD^{tree}",
        "-p",
        &main.to_string(),
        "-m",
        "after the graph",
    ]);
    repo.git(&["update-ref", "refs/heads/main", &newer]);
    let with_new = walk_all(&backend, &LogOptions::default());
    assert_eq!(
        &*with_new[0].summary, "after the graph",
        "a commit outside the graph is walked first"
    );
    assert_eq!(
        with_new.len(),
        dag.len() + 1,
        "and everything else still is"
    );
}
