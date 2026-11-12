//! The backend seam (spec 21 §2.4; owned by G0b).
//!
//! `src/backend/**` is the only place that names `gitcthulhu_git`, `gix` and the other backend crates. G0a only
//! re-exports the names its widgets and the menu model need; G0b replaces this file with the full §2.4 version,
//! which re-exports these names unchanged.

pub use bstr::{BString, ByteSlice};
pub use gitcthulhu_git::{ChangeKind, GitTime, RelaPath};
pub use gix::{ObjectId, refs::FullName};
