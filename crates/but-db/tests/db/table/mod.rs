use but_db::DbHandle;

mod ci_check;
mod fetch_status;
pub(crate) mod forge_review;

/// Return a valid DB handle with all migrations applied, ready for use, and *in-memory* only.
fn in_memory_db() -> DbHandle {
    DbHandle::new_at_path(":memory:").expect("in-memory always works")
}
