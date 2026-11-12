//! Crate-internal helpers shared by several modules. Nothing in here is public API.
#![allow(dead_code)] // The helpers are consumed by the work packages that fill in the other modules.

pub(crate) mod content;
pub(crate) mod linediff;
pub(crate) mod names;
pub(crate) mod partial;
pub(crate) mod pathspec;
pub(crate) mod post_state;

use crate::{GitError, Result};

/// The body of a function whose package has not been merged yet.
pub(crate) fn not_implemented<T>(op: &'static str) -> Result<T> {
    Err(GitError::NotImplemented { op })
}
