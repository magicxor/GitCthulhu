//! Crate-internal helpers shared by several modules. Nothing in here is public API.
// B-INT: `content`, `linediff` and `partial` are not consumed yet: diff.rs (B2) and stage.rs (B3) still carry private
// copies of the line engine and content reads. Remove this allow when they are migrated (spec 4.3).
#![allow(dead_code)]

pub(crate) mod content;
pub(crate) mod linediff;
pub(crate) mod names;
pub(crate) mod partial;
pub(crate) mod pathspec;
pub(crate) mod post_state;
