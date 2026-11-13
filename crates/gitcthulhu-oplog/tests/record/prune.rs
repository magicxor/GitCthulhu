//! A10: retention and pruning.

use std::time::Duration;

use gitcthulhu_oplog::{OplogOptions, PruneReport, RetentionPolicy};

use crate::support::{Fixture, all_entries, record_branch};

const MAIN_REF: &str = "refs/gitcthulhu/oplog/main";

/// `(seq, title, extra parents)` of every entry, oldest first. The extra parents are the commits after the first
/// parent, except for the oldest entry, which has no previous entry.
fn shape(fx: &Fixture, oplog: &gitcthulhu_oplog::Oplog) -> Vec<(u64, String, Vec<String>)> {
    let entries = all_entries(fx, oplog);
    entries
        .iter()
        .enumerate()
        .map(|(idx, e)| {
            let parents = fx.git(&["rev-list", "--parents", "-n1", &e.commit.to_string()]);
            let parents: Vec<String> = parents
                .split_whitespace()
                .skip(1 + usize::from(idx > 0))
                .map(ToOwned::to_owned)
                .collect();
            (e.id.0, e.title.clone(), parents)
        })
        .collect()
}

#[test]
fn prune_keeps_the_newest_entries_with_ids_titles_and_kept_commits() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    for i in 0..30 {
        record_branch(&fx, &oplog, &format!("b{i:02}"));
    }
    let before = shape(&fx, &oplog);
    let old_head = fx.rev(MAIN_REF);
    let report = {
        let mut guard = fx.guard();
        oplog
            .prune(
                &fx.repo(),
                Some(RetentionPolicy {
                    max_entries: 10,
                    max_age: Duration::from_secs(3600),
                }),
                guard.write_permission(),
            )
            .expect("pruning works")
    };
    assert_eq!(
        report,
        PruneReport {
            removed: 20,
            kept: 10
        }
    );
    let after = shape(&fx, &oplog);
    assert_eq!(
        after,
        before[20..].to_vec(),
        "seq, titles and extra parents are preserved"
    );
    assert_ne!(
        fx.rev(MAIN_REF),
        old_head,
        "the ref moved to the rewritten chain"
    );
    assert_eq!(
        fx.git(&["rev-list", "--count", "--first-parent", MAIN_REF])
            .trim(),
        "11",
        "10 entries, then the oldest kept entry's kept commit (the initial commit)"
    );
    assert!(oplog.can_undo(&fx.repo()).expect("reading works"));
    assert_eq!(
        oplog.state(&fx.repo()).expect("reading works").undo_depth,
        10
    );
}

#[test]
fn prune_by_age_keeps_only_the_newest_entry() {
    let fx = Fixture::new();
    let oplog = fx.oplog();
    for i in 0..3 {
        record_branch(&fx, &oplog, &format!("b{i}"));
    }
    std::thread::sleep(Duration::from_millis(5));
    let newest = all_entries(&fx, &oplog).last().expect("entries exist").id;
    let mut guard = fx.guard();
    let report = oplog
        .prune(
            &fx.repo(),
            Some(RetentionPolicy {
                max_entries: 500,
                max_age: Duration::ZERO,
            }),
            guard.write_permission(),
        )
        .expect("pruning works");
    drop(guard);
    assert_eq!(
        report,
        PruneReport {
            removed: 2,
            kept: 1
        }
    );
    let ids: Vec<_> = all_entries(&fx, &oplog).iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![newest], "the newest entry is always kept");
}

#[test]
fn finish_prunes_when_the_amortised_trigger_fires() {
    let fx = Fixture::new();
    let oplog = fx.oplog_with(OplogOptions {
        retention: RetentionPolicy {
            max_entries: 10,
            max_age: Duration::from_secs(3600),
        },
        ..OplogOptions::default()
    });
    for i in 0..20 {
        record_branch(&fx, &oplog, &format!("b{i:02}"));
    }
    assert_eq!(
        all_entries(&fx, &oplog).len(),
        20,
        "within max_entries + 10, nothing is pruned"
    );
    record_branch(&fx, &oplog, "b20");
    let entries = all_entries(&fx, &oplog);
    assert_eq!(
        entries.len(),
        10,
        "the 21st entry triggers pruning down to max_entries"
    );
    assert_eq!(
        entries.last().expect("entries").title,
        "Create branch 'b20'"
    );
}
