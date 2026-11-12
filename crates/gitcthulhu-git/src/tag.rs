//! Tags (WP-B6). Stub: bodies return `NotImplemented` until the package merges.
#![allow(unused_variables)]

use gix::{ObjectId, refs::FullName};

use crate::{
    OpContext, OpOutcome, Repo, RepoExclusive, Result, TagOptions, internal::not_implemented,
};

/// Create a lightweight or annotated tag.
pub fn create_tag(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
    target: ObjectId,
    opts: &TagOptions,
) -> Result<OpOutcome<FullName>> {
    not_implemented("tag::create_tag")
}

/// Delete a tag.
pub fn delete_tag(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
    name: &str,
) -> Result<OpOutcome<()>> {
    not_implemented("tag::delete_tag")
}

/// Pure check; also rejects an existing tag.
pub fn validate_tag_name(repo: &Repo, name: &str) -> Result<FullName> {
    not_implemented("tag::validate_tag_name")
}
