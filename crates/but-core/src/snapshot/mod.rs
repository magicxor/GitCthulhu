//! The ability to create a Git representation of diverse 'state' that can be restored at a later time.

/// Structures to call the [create_tree()] function.
pub mod create_tree;
pub use create_tree::function::create_tree;

/// Utilities related to resolving previously created snapshots.
pub mod resolve_tree;
pub use resolve_tree::function::{resolve_tree, restore_conflicts_into_index};
