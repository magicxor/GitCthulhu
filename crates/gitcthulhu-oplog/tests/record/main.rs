//! Storage and recording tests of the oplog (spec 17 §5.1 group A).

#[path = "../support/mod.rs"]
mod support;

mod capture;
mod classify;
mod prune;
mod stacks;
mod storage;
