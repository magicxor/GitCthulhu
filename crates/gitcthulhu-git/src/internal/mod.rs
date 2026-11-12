//! Crate-internal helpers shared by several modules. Nothing in here is public API.

pub(crate) mod content;
pub(crate) mod linediff;
// Not consumed yet: branch.rs, tag.rs, remote.rs and worktree.rs still validate names themselves, with rules that
// differ in detail (see the module docs). Adopting `names` changes what those modules accept, so it is their owners'
// follow-up; until then this one module is exempt.
#[allow(dead_code)]
pub(crate) mod names;
pub(crate) mod partial;
pub(crate) mod pathspec;
pub(crate) mod post_state;
