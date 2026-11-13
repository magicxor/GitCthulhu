//! Commit-graph lane layout (WP-B4, spec §3.6).
//!
//! Rows are rendered as compact ASCII, one line per row, two columns per lane, followed by the commit's subject:
//! `*` commit, `M` merge, `o` root, `S` stash, `U` uncommitted changes, `B` shallow boundary; `|` a passing lane,
//! `/` a fork-in (a line ending in the node from the top of its lane), `\` a merge-out starting a new line, `+` a
//! merge-out joining a line that already passes, `X` a lane that forks in and merges out on the same row.

#[path = "support/graphs.rs"]
mod graphs;
mod support;

use std::sync::Arc;

use gitcthulhu_git::{
    CancelToken, CommitInfo, GitTime, GraphBuilder, GraphLayout, LogOptions, LogTips, LogWalker,
    NodeKind, Person, RowView, Signature,
};
use gix::ObjectId;
use graphs::Dag;
use snapbox::IntoData as _;
use support::TestEnv;

// ───────────────────────────── helpers ─────────────────────────────

/// A synthetic, unique id for node `n`.
fn synthetic_id(n: usize) -> ObjectId {
    let mut bytes = [0u8; 20];
    bytes[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
    ObjectId::from_bytes_or_panic(&bytes)
}

fn commit(id: ObjectId, parents: impl IntoIterator<Item = ObjectId>, summary: &str) -> CommitInfo {
    let signature = Signature {
        person: Arc::new(Person {
            name: "A U Thor".into(),
            email: "author@example.com".into(),
        }),
        time: GitTime {
            seconds: 0,
            offset_minutes: 0,
        },
    };
    CommitInfo {
        id,
        parents: parents.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        summary: summary.into(),
        has_body: false,
    }
}

/// The commits of `dag` in walk order, with synthetic ids.
fn walk(dag: &Dag) -> Vec<CommitInfo> {
    dag.walk_order()
        .map(|n| {
            let node = &dag.nodes[n];
            commit(
                synthetic_id(n),
                node.parents.iter().map(|&p| synthetic_id(p)),
                &node.name,
            )
        })
        .collect()
}

/// Lay out `commits` (no flags) and return the builder.
fn layout_of(commits: &[CommitInfo]) -> GraphBuilder {
    let mut builder = GraphBuilder::new(false);
    for c in commits {
        builder.push(c, false, false);
    }
    builder
}

fn glyph(kind: NodeKind) -> char {
    match kind {
        NodeKind::Commit => '*',
        NodeKind::Merge => 'M',
        NodeKind::Root => 'o',
        NodeKind::Stash => 'S',
        NodeKind::Uncommitted => 'U',
        NodeKind::ShallowBoundary => 'B',
    }
}

/// How the GUI would draw the rows: with the uncommitted row, or hiding it (§3.5).
#[derive(Clone, Copy, PartialEq)]
enum Uncommitted {
    Shown,
    Hidden,
}

fn render_row(row: &RowView<'_>, width: usize, mode: Uncommitted) -> String {
    let mut cells = vec![' '; width];
    let hidden_link = match mode {
        Uncommitted::Hidden => row.uncommitted_link_lane,
        Uncommitted::Shown => None,
    };
    for lane in row.passing() {
        if Some(lane) != hidden_link {
            cells[usize::from(lane)] = '|';
        }
    }
    for lane in row.fork_ins() {
        cells[usize::from(lane)] = '/';
    }
    for out in row.merge_outs() {
        let cell = &mut cells[usize::from(out.lane)];
        *cell = if out.joins_existing {
            '+'
        } else if *cell == '/' {
            'X'
        } else {
            '\\'
        };
    }
    cells[usize::from(row.node_lane)] = glyph(row.kind);
    let mut line: String = cells.iter().flat_map(|c| [*c, ' ']).collect();
    line.truncate(line.trim_end().len());
    line
}

/// One line per row: lanes, then the label of the row.
fn render_with(layout: &GraphLayout, labels: &[String], mode: Uncommitted) -> String {
    let width = usize::from(layout.max_lane()) + 1;
    let first =
        if mode == Uncommitted::Hidden && labels.first().is_some_and(|l| l == "(uncommitted)") {
            1
        } else {
            0
        };
    let mut out = String::new();
    for (idx, label) in labels.iter().enumerate().take(layout.len()).skip(first) {
        let row = layout.row(idx);
        let lanes = render_row(&row, width, mode);
        out.push_str(&format!("{lanes:<width$}  {label}\n", width = width * 2));
    }
    out
}

fn render(layout: &GraphLayout, labels: &[String]) -> String {
    render_with(layout, labels, Uncommitted::Shown)
}

fn subjects(commits: &[CommitInfo]) -> Vec<String> {
    commits.iter().map(|c| c.summary.to_string()).collect()
}

/// The lanes entering a row from the top, and leaving it at the bottom.
fn top_and_bottom(row: &RowView<'_>) -> (Vec<u16>, Vec<u16>) {
    let mut top: Vec<u16> = row.passing().chain(row.fork_ins()).collect();
    let mut bottom: Vec<u16> = row
        .passing()
        .chain(
            row.merge_outs()
                .filter(|m| !m.joins_existing)
                .map(|m| m.lane),
        )
        .collect();
    if row.line_up {
        top.push(row.node_lane);
    }
    if row.line_down {
        bottom.push(row.node_lane);
    }
    top.sort_unstable();
    bottom.sort_unstable();
    (top, bottom)
}

/// The externally visible invariants of §3.3 (2, 3 and continuity, which implies the bookkeeping of invariant 1).
fn check_rows(layout: &GraphLayout) -> Result<(), String> {
    let mut previous_bottom: Vec<u16> = Vec::new();
    for idx in 0..layout.len() {
        let row = layout.row(idx);
        let passing: Vec<u16> = row.passing().collect();
        if !passing.windows(2).all(|w| w[0] < w[1]) {
            return Err(format!(
                "row {idx}: passing lanes are not ascending: {passing:?}"
            ));
        }
        if passing.contains(&row.node_lane) {
            return Err(format!(
                "row {idx}: the node lane {} also passes",
                row.node_lane
            ));
        }
        for out in row.merge_outs() {
            if out.joins_existing && !passing.contains(&out.lane) {
                return Err(format!(
                    "row {idx}: merge-out joins lane {} which does not pass",
                    out.lane
                ));
            }
        }
        let (top, bottom) = top_and_bottom(&row);
        if top != previous_bottom {
            return Err(format!(
                "row {idx}: lanes entering from the top {top:?} differ from those leaving row {} {previous_bottom:?}",
                idx.wrapping_sub(1)
            ));
        }
        let mut deduped = top.clone();
        deduped.dedup();
        if deduped.len() != top.len() {
            return Err(format!("row {idx}: a lane enters twice: {top:?}"));
        }
        if layout.max_lane_in(idx..idx + 1) > layout.max_lane() {
            return Err(format!("row {idx}: uses a lane beyond max_lane"));
        }
        previous_bottom = bottom;
    }
    Ok(())
}

/// Continuity as the GUI draws it with the uncommitted row hidden (§3.5): row 0 is skipped, the link lane is not
/// drawn where `uncommitted_link_lane` says so, and HEAD's `line_up` is dropped when it exists only for the hidden
/// row. Every line that is drawn must still connect from row to row.
fn check_rows_hidden(layout: &GraphLayout) -> Result<(), String> {
    if layout.is_empty() || layout.row(0).kind != NodeKind::Uncommitted {
        return Ok(());
    }
    let mut previous_bottom: Vec<u16> = Vec::new();
    for idx in 1..layout.len() {
        let row = layout.row(idx);
        let hidden = row.uncommitted_link_lane;
        let visible: Vec<u16> = row.passing().filter(|&l| Some(l) != hidden).collect();
        let mut top: Vec<u16> = visible.iter().copied().chain(row.fork_ins()).collect();
        if row.line_up && !row.line_up_from_uncommitted_only {
            top.push(row.node_lane);
        }
        // A merge-out that joins the hidden link lane draws its own curve to the bottom of that lane.
        let mut bottom: Vec<u16> = visible
            .iter()
            .copied()
            .chain(
                row.merge_outs()
                    .filter(|m| !m.joins_existing || Some(m.lane) == hidden)
                    .map(|m| m.lane),
            )
            .collect();
        if row.line_down {
            bottom.push(row.node_lane);
        }
        top.sort_unstable();
        bottom.sort_unstable();
        if top != previous_bottom {
            return Err(format!(
                "row {idx} (uncommitted row hidden): lanes entering from the top {top:?} differ from those leaving \
                 the row above {previous_bottom:?}"
            ));
        }
        previous_bottom = bottom;
    }
    Ok(())
}

/// A comparable snapshot of every row's public data.
fn row_data(layout: &GraphLayout, rows: std::ops::Range<usize>) -> Vec<String> {
    rows.map(|idx| {
        let row = layout.row(idx);
        format!(
            "{} {:?} up={} down={} uo={} link={:?} pass={:?} fork={:?} merge={:?}",
            row.node_lane,
            row.kind,
            row.line_up,
            row.line_down,
            row.line_up_from_uncommitted_only,
            row.uncommitted_link_lane,
            row.passing().collect::<Vec<_>>(),
            row.fork_ins().collect::<Vec<_>>(),
            row.merge_outs().collect::<Vec<_>>(),
        )
    })
    .collect()
}

/// A repository with the commits of `dag`, imported with `git fast-import`.
fn imported(env: &TestEnv, dag: &Dag) -> support::TestRepo {
    let repo = env.repo("graph");
    repo.fast_import(&dag.fast_import_stream());
    repo
}

/// HEAD, local branches only.
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

// ───────────────────────────── shapes ─────────────────────────────

#[test]
fn linear_history_single_lane() {
    let commits = walk(&graphs::linear(4));
    let builder = layout_of(&commits);
    // A line of commits stays in lane 0 and ends at the root.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
*   c3
*   c2
*   c1
o   c0

"#]]
        .raw()
    );
    assert_eq!(builder.layout().max_lane(), 0, "a line needs one lane");
    assert_eq!(builder.open_lanes(), 0, "the root ends the only line");
}

#[test]
fn simple_branch_and_merge() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let main1 = dag.add("main1", &[base]);
    let feat1 = dag.add("feat1", &[base]);
    let merge = dag.add("merge feat", &[main1, feat1]);
    dag.set_ref("refs/heads/main", merge);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // The merged branch branches out to the right and forks back into its base.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
M \   merge feat
| *   feat1
* |   main1
o /   base

"#]]
        .raw()
    );
}

#[test]
fn two_branches_from_same_parent_fork_in() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    dag.add("a", &[base]);
    let b = dag.add("b", &[base]);
    dag.set_ref("refs/heads/main", b);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // The second line to reach `base` curves into it from lane 1.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
*     b
| *   a
o /   base

"#]]
        .raw()
    );
    let base_row = builder.layout().row(2);
    assert_eq!(
        base_row.fork_ins().collect::<Vec<_>>(),
        [1],
        "lane 1 ends in base"
    );
    assert!(base_row.line_up, "lane 0 continues straight into base");
}

#[test]
fn octopus_merge_three_lanes() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let p1 = dag.add("p1", &[base]);
    let p2 = dag.add("p2", &[base]);
    let p3 = dag.add("p3", &[base]);
    let octopus = dag.add("octopus", &[p1, p2, p3]);
    dag.set_ref("refs/heads/main", octopus);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // Every parent beyond the first gets its own new lane to the right.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
M \ \   octopus
| | *   p3
| * |   p2
* | |   p1
o / /   base

"#]]
        .raw()
    );
    assert_eq!(
        builder.layout().max_lane(),
        2,
        "three parents need three lanes"
    );
    assert_eq!(
        builder.layout().row(0).kind,
        NodeKind::Merge,
        "an octopus is a merge"
    );
}

#[test]
fn merge_into_existing_lane_joins() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let x = dag.add("x", &[base]);
    let f = dag.add("f", &[x]);
    let m1 = dag.add("m1", &[x]);
    dag.add("merge f", &[m1, f]);
    let other = dag.add("other", &[f]);
    dag.set_ref("refs/heads/main", other);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // `f` is already waited for in lane 0 when the merge is laid out, so the merge joins that line.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
*     other
+ M   merge f
| *   m1
* |   f
* /   x
o     base

"#]]
        .raw()
    );
    let merge_outs: Vec<_> = builder.layout().row(1).merge_outs().collect();
    assert_eq!(merge_outs.len(), 1, "one second parent");
    assert!(
        merge_outs[0].joins_existing && merge_outs[0].lane == 0,
        "the merge joins lane 0 instead of opening a new lane"
    );
    assert_eq!(
        builder.layout().max_lane(),
        1,
        "joining needs no third lane"
    );
}

// ───────────────────────────── special nodes ─────────────────────────────

#[test]
fn stash_only_first_parent_and_helpers_hidden() {
    let env = TestEnv::new();
    let repo = env.repo("stash");
    repo.write("file.txt", "base\n");
    repo.commit_all("base");
    repo.write("file.txt", "changed\n");
    repo.write("untracked.txt", "new\n");
    repo.git(&["stash", "push", "-u", "-m", "work in progress"]);
    repo.write("file.txt", "next\n");
    let head = repo.commit_all("next");
    let backend = repo.open();
    let cancel = CancelToken::new();
    let mut walker =
        LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("walk starts");
    let page = walker.next_page(100, &cancel).expect("page loads");
    let mut builder = GraphBuilder::new(walker.first_parent());
    builder.push_uncommitted(Some(head));
    builder.push_page(&page, &walker);
    let mut labels = vec!["(uncommitted)".to_owned()];
    labels.extend(page.commits.iter().map(|c| c.summary.to_string()));
    // The stash is drawn with its first parent only; its index and untracked commits never appear.
    snapbox::assert_data_eq!(
        render(builder.layout(), &labels),
        snapbox::str![[r#"
U     (uncommitted)
*     next
| S   On main: work in progress
o /   base

"#]]
        .raw()
    );
    assert_eq!(
        builder.open_lanes(),
        0,
        "no lane waits for a stash helper commit"
    );
    check_rows(builder.layout()).expect("rows are consistent");
}

#[test]
fn uncommitted_row_puts_head_in_lane_0() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let head = dag.add("head", &[base]);
    let feature = dag.add("feature", &[base]);
    dag.set_ref("refs/heads/main", head);
    dag.set_ref("refs/heads/feature", feature);
    let commits = walk(&dag);
    let mut builder = GraphBuilder::new(false);
    builder.push_uncommitted(Some(synthetic_id(head)));
    for c in &commits {
        builder.push(c, false, false);
    }
    let mut labels = vec!["(uncommitted)".to_owned()];
    labels.extend(subjects(&commits));
    // HEAD keeps lane 0 below the uncommitted row even though a newer branch tip comes first.
    snapbox::assert_data_eq!(
        render(builder.layout(), &labels),
        snapbox::str![[r#"
U     (uncommitted)
| *   feature
* |   head
o /   base

"#]]
        .raw()
    );
    let head_row = builder.layout().row(2);
    assert_eq!(head_row.node_lane, 0, "HEAD is in lane 0");
    assert!(head_row.line_up, "the uncommitted row's line enters HEAD");
}

#[test]
fn unborn_uncommitted_row_no_line_down() {
    let mut builder = GraphBuilder::new(false);
    builder.push_uncommitted(None);
    let row = builder.layout().row(0);
    assert_eq!(
        row.kind,
        NodeKind::Uncommitted,
        "row 0 is the uncommitted row"
    );
    assert!(
        !row.line_down && !row.line_up,
        "an unborn HEAD has no commit to connect to"
    );
    assert_eq!(
        builder.open_lanes(),
        0,
        "nothing is reserved for an unborn HEAD"
    );

    // Other branches may exist while HEAD is unborn: their commits reuse lane 0.
    let other = commit(synthetic_id(1), [], "other");
    builder.push(&other, false, false);
    assert_eq!(builder.layout().row(1).node_lane, 0, "lane 0 was freed");
    assert_eq!(
        builder.layout().row(1).uncommitted_link_lane,
        None,
        "no link passes without HEAD"
    );
}

#[test]
fn root_commit_frees_lane() {
    let mut dag = Dag::default();
    let r1 = dag.add("r1", &[]);
    let c1 = dag.add("c1", &[r1]);
    let r2 = dag.add("r2", &[]);
    let d1 = dag.add("d1", &[r2]);
    let c2 = dag.add("c2", &[c1]);
    let r3 = dag.add("r3", &[]);
    dag.add("e1", &[r3]);
    dag.set_ref("refs/heads/main", c2);
    dag.set_ref("refs/heads/orphan", d1);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // The orphan line in lane 1 ends at its root `r2`; lane 1 is free again for later lines.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
*     e1
o     r3
*     c2
| *   d1
| o   r2
*     c1
o     r1

"#]]
        .raw()
    );
    assert_eq!(builder.open_lanes(), 0, "every line ended at a root");
    assert_eq!(builder.layout().max_lane(), 1, "two lanes at most");
}

#[test]
fn shallow_boundary_ends_line() {
    let commits = walk(&graphs::linear(3));
    let mut builder = GraphBuilder::new(false);
    builder.push(&commits[0], false, false);
    builder.push(&commits[1], false, true);
    let boundary = builder.layout().row(1);
    assert_eq!(
        boundary.kind,
        NodeKind::ShallowBoundary,
        "the boundary has its own glyph"
    );
    assert!(boundary.line_up, "its child connects to it");
    assert!(!boundary.line_down, "its parents are not in the repository");
    assert_eq!(
        builder.open_lanes(),
        0,
        "nothing waits for the cut-off parent"
    );
}

#[test]
fn toggle_uncommitted_visibility_needs_no_relayout() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let head = dag.add("head", &[base]);
    let child = dag.add("child of head", &[head]);
    let feature = dag.add("feature", &[base]);
    dag.set_ref("refs/heads/main", head);
    dag.set_ref("refs/heads/child", child);
    dag.set_ref("refs/heads/feature", feature);
    let commits = walk(&dag);
    let mut builder = GraphBuilder::new(false);
    builder.push_uncommitted(Some(synthetic_id(head)));
    for c in &commits {
        builder.push(c, false, false);
    }
    let layout = builder.layout();
    let mut labels = vec!["(uncommitted)".to_owned()];
    labels.extend(subjects(&commits));

    let head_row = 1 + commits
        .iter()
        .position(|c| c.id == synthetic_id(head))
        .expect("HEAD was walked");
    for idx in 0..layout.len() {
        let row = layout.row(idx);
        assert_eq!(
            row.line_up_from_uncommitted_only,
            idx == head_row,
            "row {idx}: only HEAD's row has a line up that exists only for the uncommitted row"
        );
        let expected_link = (idx > 0 && idx < head_row).then_some(0);
        assert_eq!(
            row.uncommitted_link_lane, expected_link,
            "row {idx}: the link lane passes exactly the rows between row 0 and HEAD"
        );
    }
    // Shown: lane 0 connects the uncommitted row with HEAD.
    snapbox::assert_data_eq!(
        render(layout, &labels),
        snapbox::str![[r#"
U       (uncommitted)
| *     feature
| | *   child of head
* | /   head
o /     base

"#]]
        .raw()
    );
    // Hidden: the same layout, drawn without row 0 and without the link lane; HEAD's other child still forks in.
    snapbox::assert_data_eq!(
        render_with(layout, &labels, Uncommitted::Hidden),
        snapbox::str![[r#"
  *     feature
  | *   child of head
* | /   head
o /     base

"#]]
        .raw()
    );
    check_rows(layout).expect("rows are consistent");
    check_rows_hidden(layout).expect("hiding the uncommitted row leaves no broken line");
}

#[test]
fn merge_joining_head_lane_stays_visible_when_hidden() {
    // HEAD is a feature branch that `main` already merged: the merge's second parent is HEAD, so its merge-out joins
    // the lane the uncommitted row reserved for HEAD.
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let feature = dag.add("feature (HEAD)", &[base]);
    let m1 = dag.add("m1", &[base]);
    let m2 = dag.add("m2", &[m1]);
    let merge = dag.add("merge feature", &[m2, feature]);
    dag.set_ref("refs/heads/main", merge);
    dag.set_ref("refs/heads/feature", feature);
    let commits = walk(&dag);
    let mut builder = GraphBuilder::new(false);
    builder.push_uncommitted(Some(synthetic_id(feature)));
    for c in &commits {
        builder.push(c, false, false);
    }
    let layout = builder.layout();
    let mut labels = vec!["(uncommitted)".to_owned()];
    labels.extend(subjects(&commits));

    snapbox::assert_data_eq!(
        render(layout, &labels),
        snapbox::str![[r#"
U     (uncommitted)
+ M   merge feature
| *   m2
| *   m1
* |   feature (HEAD)
o /   base

"#]]
        .raw()
    );
    // Hidden: the merge's line into HEAD is drawn from the merge row down; only the link above it disappears.
    snapbox::assert_data_eq!(
        render_with(layout, &labels, Uncommitted::Hidden),
        snapbox::str![[r#"
+ M   merge feature
| *   m2
| *   m1
* |   feature (HEAD)
o /   base

"#]]
        .raw()
    );
    assert_eq!(
        layout.row(1).uncommitted_link_lane,
        Some(0),
        "above the join, lane 0 is only the link"
    );
    assert_eq!(
        (
            layout.row(2).uncommitted_link_lane,
            layout.row(3).uncommitted_link_lane
        ),
        (None, None),
        "below the join, lane 0 carries the merge's line"
    );
    let head_row = layout.row(4);
    assert!(
        head_row.line_up && !head_row.line_up_from_uncommitted_only,
        "HEAD's line up comes from the merge, so it stays when the uncommitted row is hidden"
    );
    check_rows(layout).expect("rows are consistent");
    check_rows_hidden(layout).expect("hiding the uncommitted row leaves no broken line");
}

#[test]
fn first_parent_mode_has_no_dangling_lanes() {
    let env = TestEnv::new();
    let dag = graphs::mainline_with_merges(50);
    let repo = imported(&env, &dag);
    let backend = repo.open();
    let cancel = CancelToken::new();
    let opts = LogOptions {
        first_parent: true,
        ..branches_only()
    };
    let mut walker = LogWalker::new(&backend, &opts, &cancel).expect("walk starts");
    let mut builder = GraphBuilder::new(walker.first_parent());
    while !walker.is_exhausted() {
        let page = walker.next_page(17, &cancel).expect("page loads");
        builder.push_page(&page, &walker);
    }
    assert_eq!(
        builder.layout().len(),
        101,
        "root plus two first-parent commits per merge"
    );
    assert_eq!(
        builder.open_lanes(),
        0,
        "no lane waits for a second parent the walk never emits"
    );
    assert_eq!(
        builder.layout().max_lane(),
        0,
        "the first-parent chain is a single line"
    );
    assert_eq!(
        builder.layout().row(0).kind,
        NodeKind::Merge,
        "merges keep their glyph"
    );
}

#[test]
fn duplicate_parents_deduped() {
    let a = commit(synthetic_id(1), [], "a");
    let corrupt = commit(
        synthetic_id(2),
        [synthetic_id(1), synthetic_id(1)],
        "corrupt",
    );
    let builder = layout_of(&[corrupt, a]);
    let row = builder.layout().row(0);
    assert_eq!(
        row.kind,
        NodeKind::Commit,
        "one distinct parent is no merge"
    );
    assert_eq!(
        row.merge_outs().count(),
        0,
        "the duplicate does not join its own line"
    );
    assert_eq!(builder.open_lanes(), 0, "nothing dangles");
    check_rows(builder.layout()).expect("rows are consistent");
}

// ───────────────────────────── invariants and widths ─────────────────────────────

#[test]
fn lanes_reused_smallest_free() {
    let mut dag = Dag::default();
    let base = dag.add("base", &[]);
    let e = dag.add("e", &[base]);
    let x = dag.add("x", &[]);
    dag.add("c", &[base]);
    let y = dag.add("y", &[x]);
    let d = dag.add("d", &[base]);
    dag.set_ref("refs/heads/main", d);
    dag.set_ref("refs/heads/y", y);
    dag.set_ref("refs/heads/e", e);
    let commits = walk(&dag);
    let builder = layout_of(&commits);
    // `x` frees lane 1 between two occupied lanes; the next new line (`e`) takes it instead of opening lane 3.
    snapbox::assert_data_eq!(
        render(builder.layout(), &subjects(&commits)),
        snapbox::str![[r#"
*       d
| *     y
| | *   c
| o |   x
| * |   e
o / /   base

"#]]
        .raw()
    );
    assert_eq!(
        builder.layout().row(4).node_lane,
        1,
        "the smallest free lane is reused"
    );
}

#[test]
fn max_lane_monotonic_across_pages() {
    // A long narrow (older) tail below a wide part of 8 branches.
    let mut dag = Dag::default();
    let mut tip = dag.add("tail0", &[]);
    for k in 1..20 {
        tip = dag.add(format!("tail{k}"), &[tip]);
    }
    for k in 0..8 {
        dag.add(format!("w{k}"), &[tip]);
    }
    let commits = walk(&dag);
    let mut builder = GraphBuilder::new(false);
    let mut previous = 0;
    for page in commits.rchunks(5).rev() {
        let start = builder.layout().len();
        for c in page {
            builder.push(c, false, false);
        }
        let layout = builder.layout();
        assert!(layout.max_lane() >= previous, "max_lane never shrinks");
        assert!(
            layout.max_lane() >= layout.max_lane_in(start..layout.len()),
            "the newest page is never wider than all rows"
        );
        previous = layout.max_lane();
    }
    let layout = builder.layout();
    assert_eq!(layout.max_lane(), 7, "the wide part needs 8 lanes");
    assert_eq!(
        layout.max_lane_in(layout.len() - 5..layout.len()),
        0,
        "the narrow tail alone needs one lane"
    );
    assert_eq!(
        layout.max_lane_in(30..1000),
        0,
        "ranges are clamped to the layout"
    );
}

#[test]
fn pagination_equals_single_pass() {
    let env = TestEnv::new();
    let dag = graphs::branchy(10_000, 12, 7);
    let repo = imported(&env, &dag);
    let backend = repo.open();
    let cancel = CancelToken::new();
    let head = ObjectId::from_hex(repo.git(&["rev-parse", "HEAD"]).as_bytes()).expect("hex id");

    let layout_in_pages = |pages: &[usize]| {
        let mut walker =
            LogWalker::new(&backend, &LogOptions::default(), &cancel).expect("walk starts");
        let mut builder = GraphBuilder::new(walker.first_parent());
        builder.push_uncommitted(Some(head));
        for &n in pages {
            let page = walker.next_page(n, &cancel).expect("page loads");
            builder.push_page(&page, &walker);
        }
        assert!(walker.is_exhausted(), "the pages cover the whole walk");
        builder
    };
    let paged = layout_in_pages(&[2000, 500, 7500]);
    let single = layout_in_pages(&[10_000]);
    assert_eq!(
        single.layout().len(),
        10_001,
        "every commit plus the uncommitted row"
    );
    assert!(
        paged.layout() == single.layout(),
        "three pages produce the byte-identical layout of one pass"
    );
    assert_eq!(
        paged.open_lanes(),
        single.open_lanes(),
        "the same lanes stay open"
    );
    check_rows(single.layout()).expect("rows are consistent");
}

#[test]
fn wide_graph_spill_lanes_above_64() {
    let commits = walk(&graphs::wide(70));
    let builder = layout_of(&commits);
    let layout = builder.layout();
    assert_eq!(layout.max_lane(), 69, "70 concurrent lines");
    let last_tip = layout.row(69);
    assert_eq!(last_tip.node_lane, 69, "each new tip opens the next lane");
    assert_eq!(
        last_tip.passing().collect::<Vec<_>>(),
        (0..69).collect::<Vec<u16>>(),
        "lanes beyond 63 come from the spill pool, in ascending order"
    );
    let base = layout.row(70);
    assert_eq!(
        base.fork_ins().collect::<Vec<_>>(),
        (1..70).collect::<Vec<u16>>(),
        "every other line forks into the base"
    );
    assert_eq!(
        layout.max_lane_in(69..70),
        69,
        "the window width sees spill lanes"
    );
    check_rows(layout).expect("rows are consistent");
}

proptest::proptest! {
    /// Invariants 1-4 on random DAGs walked children-first: continuity of every lane between rows (which is only
    /// possible with exact lane bookkeeping), the node lane never passing, joins only into passing lanes, rows never
    /// changing once emitted, and a monotonic `max_lane`.
    #[test]
    fn invariants_hold_on_random_dags(
        raw in proptest::collection::vec(proptest::collection::vec(0usize..1000, 0..4), 1..150),
        with_head in proptest::bool::ANY,
        head_pick in 0usize..1000,
    ) {
        let n = raw.len();
        // Commit i (in walk order) picks parents among the later commits, sometimes one outside the walk.
        let commits: Vec<CommitInfo> = raw
            .iter()
            .enumerate()
            .map(|(i, picks)| {
                let parents = picks.iter().map(|&p| {
                    if p % 23 == 0 || i + 1 == n { synthetic_id(n + p) } else { synthetic_id(i + 1 + p % (n - i - 1)) }
                });
                commit(synthetic_id(i), parents, "c")
            })
            .collect();
        let mut builder = GraphBuilder::new(false);
        // HEAD is any walked commit, not necessarily the newest one.
        let head = synthetic_id(head_pick % n);
        if with_head {
            builder.push_uncommitted(Some(head));
        }
        let half = commits.len() / 2;
        let mut max_lane = 0;
        for c in &commits[..half] {
            builder.push(c, false, false);
            proptest::prop_assert!(builder.layout().max_lane() >= max_lane, "max_lane never decreases");
            max_lane = builder.layout().max_lane();
        }
        let early_rows = row_data(builder.layout(), 0..builder.layout().len());
        for c in &commits[half..] {
            builder.push(c, false, false);
            proptest::prop_assert!(builder.layout().max_lane() >= max_lane, "max_lane never decreases");
            max_lane = builder.layout().max_lane();
        }
        let layout = builder.layout();
        proptest::prop_assert_eq!(&row_data(layout, 0..early_rows.len()), &early_rows, "emitted rows are final");
        proptest::prop_assert_eq!(check_rows(layout), Ok(()));
        proptest::prop_assert_eq!(check_rows_hidden(layout), Ok(()));
        if with_head {
            // The link flags never reach HEAD's row or below it.
            let head_row = 1 + commits.iter().position(|c| c.id == head).expect("HEAD is walked");
            for idx in head_row..layout.len() {
                proptest::prop_assert_eq!(layout.row(idx).uncommitted_link_lane, None, "row {} is not above HEAD", idx);
            }
            for idx in 0..layout.len() {
                proptest::prop_assert!(
                    !layout.row(idx).line_up_from_uncommitted_only || idx == head_row,
                    "row {} is not HEAD's row", idx
                );
            }
        }
    }
}

// ───────────────────────────── performance ─────────────────────────────

#[test]
#[ignore = "performance: run in release with GITCTHULHU_PERF_ASSERT=1 to assert the budget"]
fn perf_100k_rows_under_budget() {
    let commits = walk(&graphs::branchy(100_000, 20, 42));
    let started = std::time::Instant::now();
    let mut builder = GraphBuilder::new(false);
    builder.push_uncommitted(Some(commits[0].id));
    for c in &commits {
        builder.push(c, false, false);
    }
    let elapsed = started.elapsed();
    println!(
        "graph layout: {} rows in {elapsed:?}, max lane {}",
        builder.layout().len(),
        builder.layout().max_lane()
    );
    if std::env::var_os("GITCTHULHU_PERF_ASSERT").is_some_and(|v| v == "1") {
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "100k rows lay out in under 100 ms (≥ 1M rows/s), took {elapsed:?}"
        );
    }
}
