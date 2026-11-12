//! Generators of commit graphs (10k-100k commits, octopus merges, wide graphs, many refs) as `git fast-import`
//! streams with deterministic dates, for the log and graph tests (WP-B4).
//!
//! A [`Dag`] lists its commits in *emission order*: parents before children, with strictly increasing committer
//! times. Its reverse is therefore both a valid walk order (children first) and exactly what
//! `git rev-list --date-order` produces, so in-memory graph tests and walks over the imported repository see the
//! same order.
//!
//! Include it next to `mod support;` with `#[path = "support/graphs.rs"] mod graphs;`.
#![allow(dead_code)] // Each test binary uses a different part of it.

use std::fmt::Write as _;

/// The committer time of commit 0; commit `i` is `i` minutes later.
pub const BASE_TIME: i64 = 1_600_000_000;

/// One commit of a [`Dag`].
#[derive(Debug, Clone)]
pub struct Node {
    /// Indices of the parents (all smaller than this node's index), first parent first.
    pub parents: Vec<usize>,
    /// The commit message's subject; unique within the graph.
    pub name: String,
}

/// A commit graph in emission order (parents first), plus the refs pointing into it.
#[derive(Debug, Clone, Default)]
pub struct Dag {
    pub nodes: Vec<Node>,
    /// `(full ref name, node)`; must include `refs/heads/main` (HEAD of a fresh fixture repository).
    pub refs: Vec<(String, usize)>,
}

impl Dag {
    /// Append a commit and return its index.
    pub fn add(&mut self, name: impl Into<String>, parents: &[usize]) -> usize {
        let idx = self.nodes.len();
        assert!(
            parents.iter().all(|&p| p < idx),
            "parents are emitted before their children"
        );
        self.nodes.push(Node {
            parents: parents.to_vec(),
            name: name.into(),
        });
        idx
    }

    /// Point `full_name` at `node`.
    pub fn set_ref(&mut self, full_name: impl Into<String>, node: usize) {
        let full_name = full_name.into();
        self.refs.retain(|(name, _)| *name != full_name);
        self.refs.push((full_name, node));
    }

    /// Number of commits.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The node indices in walk order (children first, newest first).
    pub fn walk_order(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.nodes.len()).rev()
    }

    /// The committer (and author) time of `node`.
    pub fn time(node: usize) -> i64 {
        BASE_TIME + node as i64 * 60
    }

    /// The `git fast-import` stream creating every commit (empty trees) and every ref.
    pub fn fast_import_stream(&self) -> Vec<u8> {
        assert!(
            self.refs.iter().any(|(name, _)| name == "refs/heads/main"),
            "the fixture repository's HEAD is refs/heads/main, so the graph must set it"
        );
        let mut out = String::with_capacity(self.nodes.len() * 160);
        for (idx, node) in self.nodes.iter().enumerate() {
            if node.parents.is_empty() {
                // Without `from`, fast-import would continue from the branch's current tip.
                out.push_str("reset refs/heads/main\n\n");
            }
            let time = Self::time(idx);
            let message = format!("{}\n", node.name);
            writeln!(out, "commit refs/heads/main").ok();
            writeln!(out, "mark :{}", idx + 1).ok();
            writeln!(out, "author A U Thor <author@example.com> {time} +0000").ok();
            writeln!(
                out,
                "committer C O Mitter <committer@example.com> {time} +0000"
            )
            .ok();
            writeln!(out, "data {}\n{message}", message.len()).ok();
            for (n, parent) in node.parents.iter().enumerate() {
                let verb = if n == 0 { "from" } else { "merge" };
                writeln!(out, "{verb} :{}", parent + 1).ok();
            }
            out.push('\n');
        }
        for (name, node) in &self.refs {
            writeln!(out, "reset {name}\nfrom :{}\n", node + 1).ok();
        }
        out.into_bytes()
    }
}

/// A tiny deterministic PRNG (xorshift64*), so generated graphs are identical on every run and platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// `n` commits in a line; `main` at the newest.
pub fn linear(n: usize) -> Dag {
    let mut dag = Dag::default();
    for idx in 0..n {
        let parents: Vec<usize> = idx.checked_sub(1).into_iter().collect();
        dag.add(format!("c{idx}"), &parents);
    }
    dag.set_ref("refs/heads/main", n - 1);
    dag
}

/// A realistic history of `n` commits: a mainline with up to `max_open` feature branches that fork from main,
/// receive commits, sometimes merge main back in, and are merged into main (sometimes as octopus merges). Branches
/// still open at the end get refs `refs/heads/b<k>`; every 500th mainline commit gets a tag `refs/tags/v<k>`.
pub fn branchy(n: usize, max_open: usize, seed: u64) -> Dag {
    let mut rng = Rng::new(seed);
    let mut dag = Dag::default();
    let mut main = dag.add("main-0", &[]);
    let mut open: Vec<(usize, usize)> = Vec::new(); // (branch number, tip)
    let mut next_branch = 0;
    let mut mainline = 0;
    while dag.len() < n {
        match rng.below(10) {
            // Commit on main.
            0..=2 => {
                mainline += 1;
                main = dag.add(format!("main-{mainline}"), &[main]);
                if mainline % 500 == 0 {
                    dag.set_ref(format!("refs/tags/v{}", mainline / 500), main);
                }
            }
            // Start a branch.
            3 if open.len() < max_open => {
                let tip = dag.add(format!("b{next_branch}-0"), &[main]);
                open.push((next_branch, tip));
                next_branch += 1;
            }
            // Merge one (or, rarely, three) branches into main.
            4 | 5 if !open.is_empty() => {
                let take = if open.len() >= 3 && rng.below(8) == 0 {
                    3
                } else {
                    1
                };
                let mut parents = vec![main];
                let mut names = Vec::new();
                for _ in 0..take {
                    let (branch, tip) = open.swap_remove(rng.below(open.len()));
                    parents.push(tip);
                    names.push(format!("b{branch}"));
                }
                main = dag.add(format!("merge {}", names.join(" ")), &parents);
            }
            // Merge main into a branch.
            6 if !open.is_empty() && rng.below(4) == 0 => {
                let pick = rng.below(open.len());
                let (branch, tip) = open[pick];
                let merged = dag.add(format!("b{branch}-sync-{}", dag.len()), &[tip, main]);
                open[pick].1 = merged;
            }
            // Commit on a branch.
            _ if !open.is_empty() => {
                let pick = rng.below(open.len());
                let (branch, tip) = open[pick];
                open[pick].1 = dag.add(format!("b{branch}-{}", dag.len()), &[tip]);
            }
            _ => {
                mainline += 1;
                main = dag.add(format!("main-{mainline}"), &[main]);
            }
        }
    }
    dag.set_ref("refs/heads/main", main);
    for (branch, tip) in open {
        dag.set_ref(format!("refs/heads/b{branch}"), tip);
    }
    dag
}

/// A mainline with `merges` merge commits, each merging a two-commit side branch that has no ref of its own.
pub fn mainline_with_merges(merges: usize) -> Dag {
    let mut dag = Dag::default();
    let mut main = dag.add("root", &[]);
    for k in 0..merges {
        let side1 = dag.add(format!("side{k}-1"), &[main]);
        let side2 = dag.add(format!("side{k}-2"), &[side1]);
        let work = dag.add(format!("main{k}"), &[main]);
        main = dag.add(format!("merge{k}"), &[work, side2]);
    }
    dag.set_ref("refs/heads/main", main);
    dag
}

/// `width` branches forking from one base commit, each with one commit and a ref `refs/heads/w<k>`, all open at once;
/// `main` stays at the base.
pub fn wide(width: usize) -> Dag {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    for k in 0..width {
        let tip = dag.add(format!("w{k}"), &[base]);
        dag.set_ref(format!("refs/heads/w{k}"), tip);
    }
    dag.set_ref("refs/heads/main", base);
    dag
}
