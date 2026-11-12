//! Repository tab entities (owned by G0b).
//!
//! G0a only provides the two identifiers that the visual foundation needs (`TabId` for toasts, `TabCtx` for the
//! menu extension point). G0b replaces this file with the full §2.1/§2.3 definitions; `TabCtx` keeps its `id`
//! field.

/// Identifies a repository tab for the lifetime of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// What a region view needs from its tab. Cheap to clone.
#[derive(Clone, Debug)]
pub struct TabCtx {
    pub id: TabId,
}
