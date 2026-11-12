//! Retention: dropping the oldest entries of a chain (spec 17 §2.5).

use gix::refs::FullNameRef;

use crate::{
    OplogError, RetentionPolicy,
    storage::{self, Chain},
    types::PruneReport,
};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

fn duration_ms(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// The amortised trigger: prune only when the chain is clearly over the limits, so that `finish()` rewrites the
/// chain at most once every ~`max(10, max_entries / 10)` operations.
pub(crate) fn is_due(chain: &Chain, policy: &RetentionPolicy, now_ms: i64) -> bool {
    let slack = (policy.max_entries / 10).max(10);
    if chain.entries.len() > policy.max_entries.saturating_add(slack) {
        return true;
    }
    let grace = duration_ms(policy.max_age).saturating_add(DAY_MS);
    chain
        .entries
        .first()
        .is_some_and(|oldest| oldest.meta.finished_at_ms < now_ms.saturating_sub(grace))
}

/// How many of the newest entries satisfy both limits; at least one (the newest is always kept).
fn kept_count(chain: &Chain, policy: &RetentionPolicy, now_ms: i64) -> usize {
    let cutoff = now_ms.saturating_sub(duration_ms(policy.max_age));
    let within = chain
        .entries
        .iter()
        .rev()
        .take(policy.max_entries)
        .take_while(|e| e.meta.finished_at_ms >= cutoff)
        .count();
    within.max(1).min(chain.entries.len())
}

/// Apply `policy` to `chain` (the chain `ref_name` points to now): rewrite the kept entries onto a new root and move
/// the ref with a guard. Sequence numbers, trees, messages and extra parents are preserved.
pub(crate) fn prune(
    repo: &gix::Repository,
    ref_name: &FullNameRef,
    chain: &Chain,
    policy: &RetentionPolicy,
    now_ms: i64,
) -> Result<PruneReport, OplogError> {
    let total = chain.entries.len();
    let kept = kept_count(chain, policy, now_ms);
    let removed = total - kept;
    if removed == 0 {
        return Ok(PruneReport { removed: 0, kept });
    }
    let mut new_head = None;
    for (idx, entry) in chain.entries.iter().enumerate().skip(removed) {
        let rewritten = storage::rewrite_entry_commit(repo, entry.commit, idx > 0, new_head)
            .map_err(|err| {
                OplogError::Other(err.context(format!(
                    "rewriting oplog entry {} while pruning",
                    entry.meta.seq
                )))
            })?;
        new_head = Some(rewritten);
    }
    let new_head = new_head.expect("at least one entry is kept");
    storage::update_ref(repo, ref_name, chain.head, new_head)?;
    Ok(PruneReport { removed, kept })
}
