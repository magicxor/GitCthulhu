//! Seeded repository models for layout and behaviour tests without a backend (spec 21 §8.1). Region-specific seeds
//! are free functions in the regions' own `tests.rs`.

use std::sync::Arc;

use gitcthulhu_git::{RepoFlags, StatusFingerprint};
use gpui_kit::{Entity, TestAppContext};

use crate::backend::{
    ChangeKind, Decorations, FullName, GitTime, HeadState, Identity, IdentityInfo, InProgress,
    KindCounts, LocalBranch, MergeOut, NodeKind, ObjectId, ObjectKind, PartialIdentity, RefList,
    RelaPath, RemoteBranch, RemoteInfo, RepoState, StashInfo, Status, StatusCounts, StatusEntry,
    SubmoduleInfo, TagInfo, UndoRedoState, WorktreeInfo,
};
use crate::tab::model::{
    ChunkedRows, GraphCell, Loadable, LogCommit, LogRow, LogSnapshot, RepoModel,
};

/// A synthetic object id: `n` as 40 hex digits.
pub fn oid(n: u64) -> ObjectId {
    ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("40 hex digits are an object id")
}

/// A full ref name.
pub fn full(name: &str) -> FullName {
    FullName::try_from(name).expect("a valid full ref name")
}

/// A status entry of `kind` at `path`.
pub fn entry(path: &str, kind: ChangeKind) -> StatusEntry {
    StatusEntry {
        path: RelaPath::from(path),
        previous_path: None,
        kind,
        old: None,
        new: None,
        conflict: None,
        submodule: None,
    }
}

fn counts_of(entries: &[StatusEntry]) -> KindCounts {
    let mut counts = KindCounts::default();
    for e in entries {
        match e.kind {
            ChangeKind::Added | ChangeKind::IntentToAdd => counts.added += 1,
            ChangeKind::Modified | ChangeKind::TypeChanged => counts.modified += 1,
            ChangeKind::Deleted => counts.deleted += 1,
            ChangeKind::Renamed => counts.renamed += 1,
            ChangeKind::Untracked => counts.untracked += 1,
            ChangeKind::Conflicted => counts.conflicted += 1,
        }
    }
    counts
}

/// A status with these lists (counts computed).
pub fn status(staged: Vec<StatusEntry>, unstaged: Vec<StatusEntry>) -> Status {
    Status {
        counts: StatusCounts {
            staged: counts_of(&staged),
            unstaged: counts_of(&unstaged),
        },
        staged,
        unstaged,
        fingerprint: StatusFingerprint {
            index: None,
            head: None,
        },
        index_needs_refresh: false,
    }
}

fn commit(id: ObjectId, summary: &str, time: i64, parents: u8, is_stash: bool) -> LogCommit {
    LogCommit {
        id,
        short_id: id.to_hex_with_len(7).to_string().into(),
        summary: summary.to_owned().into(),
        author_name: "Test User".into(),
        author_email: "test@example.com".into(),
        committer_name: "Test User".into(),
        committer_email: "test@example.com".into(),
        committer_time: GitTime {
            seconds: time,
            offset_minutes: 0,
        },
        parent_count: parents,
        has_body: false,
        is_stash,
    }
}

fn uncommitted_row() -> LogRow {
    let mut graph = GraphCell::node(0, NodeKind::Uncommitted);
    graph.line_down = true;
    LogRow {
        commit: None,
        graph,
    }
}

fn local(name: &str, target: ObjectId, is_head: bool) -> LocalBranch {
    LocalBranch {
        name: name.to_owned(),
        full: full(&format!("refs/heads/{name}")),
        target,
        is_head,
        upstream: None,
        ahead_behind: None,
        checked_out_in: None,
    }
}

/// Everything the model of a seeded tab holds.
#[derive(Clone)]
pub struct RepoSeed {
    pub name: String,
    pub status: Status,
    pub state: RepoState,
    pub refs: RefList,
    /// Layout rows; row 0 is the uncommitted row.
    pub log_rows: Vec<LogRow>,
    pub submodules: Vec<SubmoduleInfo>,
    pub worktrees: Vec<WorktreeInfo>,
    pub identity: IdentityInfo,
    pub undo: Option<UndoRedoState>,
}

impl RepoSeed {
    /// A clean repository with one commit on `main`.
    pub fn empty() -> Self {
        Self::linear(1)
    }

    /// `n` commits on `main` (newest first), lane 0 only.
    pub fn linear(n: usize) -> Self {
        let head = oid(1);
        let mut rows = vec![uncommitted_row()];
        for i in 0..n {
            let last = i + 1 == n;
            let mut graph = GraphCell::node(
                0,
                if last {
                    NodeKind::Root
                } else {
                    NodeKind::Commit
                },
            );
            graph.line_up = true;
            graph.line_up_from_uncommitted_only = i == 0;
            graph.line_down = !last;
            rows.push(LogRow {
                commit: Some(commit(
                    oid(i as u64 + 1),
                    &format!("Commit {}", n - i),
                    1_700_000_000 - i as i64 * 60,
                    u8::from(!last),
                    false,
                )),
                graph,
            });
        }
        let head_state = HeadState::Branch {
            name: full("refs/heads/main"),
            target: head,
        };
        Self {
            name: "repo".to_owned(),
            status: status(Vec::new(), Vec::new()),
            state: RepoState {
                head: head_state.clone(),
                in_progress: None,
                merge: None,
                sequencer: None,
                rebase: None,
                bisect: None,
                conflicted_paths: 0,
                flags: RepoFlags::default(),
            },
            refs: RefList {
                head: head_state,
                local: vec![local("main", head, true)],
                remotes: Vec::new(),
                tags: Vec::new(),
                stashes: Vec::new(),
                decorations: Decorations::default(),
            },
            log_rows: rows,
            submodules: Vec::new(),
            worktrees: Vec::new(),
            identity: IdentityInfo {
                global: PartialIdentity {
                    name: Some("Test User".into()),
                    email: Some("test@example.com".into()),
                },
                local: PartialIdentity::default(),
                effective: Some(Identity {
                    name: "Test User".into(),
                    email: "test@example.com".into(),
                }),
            },
            undo: None,
        }
    }

    /// `main` with a merged `feature` branch, a tag, a stash and a remote branch; hand-built graph cells for the
    /// merge, fork, stash and root shapes of 20 §3.6.
    pub fn branchy() -> Self {
        let mut seed = Self::linear(1);
        let (merge, feature, main2, stash, main1, root) =
            (oid(10), oid(11), oid(12), oid(13), oid(14), oid(15));
        let mut rows = vec![uncommitted_row()];
        let mut g = GraphCell::node(0, NodeKind::Merge);
        g.line_up = true;
        g.line_up_from_uncommitted_only = true;
        g.line_down = true;
        g.merge_outs.push(MergeOut {
            lane: 1,
            joins_existing: false,
        });
        rows.push(LogRow {
            commit: Some(commit(
                merge,
                "Merge branch 'feature'",
                1_700_000_600,
                2,
                false,
            )),
            graph: g,
        });
        let mut g = GraphCell::node(1, NodeKind::Commit);
        g.line_up = true;
        g.line_down = true;
        g.passing.push(0);
        rows.push(LogRow {
            commit: Some(commit(feature, "Feature work", 1_700_000_500, 1, false)),
            graph: g,
        });
        let mut g = GraphCell::node(0, NodeKind::Commit);
        g.line_up = true;
        g.line_down = true;
        g.passing.push(1);
        rows.push(LogRow {
            commit: Some(commit(main2, "Main work", 1_700_000_400, 1, false)),
            graph: g,
        });
        let mut g = GraphCell::node(2, NodeKind::Stash);
        g.line_down = true;
        g.passing.extend([0, 1]);
        rows.push(LogRow {
            commit: Some(commit(stash, "WIP on main", 1_700_000_300, 3, true)),
            graph: g,
        });
        let mut g = GraphCell::node(0, NodeKind::Commit);
        g.line_up = true;
        g.line_down = true;
        g.fork_ins.extend([1, 2]);
        rows.push(LogRow {
            commit: Some(commit(main1, "Base", 1_700_000_200, 1, false)),
            graph: g,
        });
        let mut g = GraphCell::node(0, NodeKind::Root);
        g.line_up = true;
        rows.push(LogRow {
            commit: Some(commit(root, "Initial commit", 1_700_000_100, 0, false)),
            graph: g,
        });
        seed.log_rows = rows;
        let head_state = HeadState::Branch {
            name: full("refs/heads/main"),
            target: merge,
        };
        seed.state.head = head_state.clone();
        seed.refs = RefList {
            head: head_state,
            local: vec![local("feature", feature, false), local("main", merge, true)],
            remotes: vec![RemoteInfo {
                name: "origin".into(),
                fetch_url: Some("https://example.com/repo.git".into()),
                push_url: None,
                head_branch: Some("main".into()),
                branches: vec![RemoteBranch {
                    remote: "origin".into(),
                    name: "main".into(),
                    full: full("refs/remotes/origin/main"),
                    target: main2,
                }],
            }],
            tags: vec![TagInfo {
                name: "v1.0".into(),
                full: full("refs/tags/v1.0"),
                target: main1,
                peeled_kind: ObjectKind::Commit,
                tag_object: None,
                message_summary: None,
                tagger_time: None,
            }],
            stashes: vec![StashInfo {
                index: 0,
                id: stash,
                message: "WIP on main".into(),
                branch: Some("main".into()),
                time: GitTime {
                    seconds: 1_700_000_300,
                    offset_minutes: 0,
                },
                base: main1,
                index_commit: None,
                untracked_commit: None,
            }],
            decorations: Decorations::default(),
        };
        seed
    }

    /// Adds staged and unstaged changes.
    pub fn with_changes(
        mut self,
        staged: &[(&str, ChangeKind)],
        unstaged: &[(&str, ChangeKind)],
    ) -> Self {
        let mut staged_entries = self.status.staged.clone();
        staged_entries.extend(staged.iter().map(|(p, k)| entry(p, *k)));
        let mut unstaged_entries = self.status.unstaged.clone();
        unstaged_entries.extend(unstaged.iter().map(|(p, k)| entry(p, *k)));
        self.status = status(staged_entries, unstaged_entries);
        self
    }

    /// Puts the repository into the in-progress state `s`.
    pub fn in_state(mut self, s: InProgress) -> Self {
        self.state.in_progress = Some(s);
        self
    }

    /// The log snapshot of the seed's rows (generation 1).
    pub fn snapshot(&self) -> LogSnapshot {
        let max_lane = self
            .log_rows
            .iter()
            .map(|r| {
                r.graph
                    .passing
                    .iter()
                    .chain(r.graph.fork_ins.iter())
                    .copied()
                    .chain(r.graph.merge_outs.iter().map(|m| m.lane))
                    .chain([r.graph.node_lane])
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        LogSnapshot {
            rows: ChunkedRows::new().appended(self.log_rows.clone()),
            max_lane,
            exhausted: true,
            generation: 1,
        }
    }

    /// Writes the seed into `model` (one event per category).
    pub fn apply(&self, model: &Entity<RepoModel>, cx: &mut TestAppContext) {
        let seed = self.clone();
        model.update(cx, |m, cx| {
            m.set_status(Arc::new(seed.status.clone()), cx);
            m.set_state(Arc::new(seed.state.clone()), cx);
            m.set_refs(Arc::new(seed.refs.clone()), cx);
            m.set_log(Arc::new(seed.snapshot()), cx);
            m.submodules = Loadable::ready(Arc::new(seed.submodules.clone()));
            m.worktrees = Loadable::ready(Arc::new(seed.worktrees.clone()));
            m.identity = Loadable::ready(Arc::new(seed.identity.clone()));
            m.undo = seed.undo.clone();
            cx.notify();
        });
    }
}
