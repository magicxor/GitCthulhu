//! Tag operations of the catalogue (spec 21 §2.11; owned by G3): create and delete. Both are ref-only snapshots
//! (17 §4.4).

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec};
use crate::backend::{
    ObjectId, OperationDescriptor, OperationKind, RefreshSet, Scope, TagOptions, UntrackedCapture,
    git,
};
use crate::tab::RepoTab;

impl RepoTab {
    /// Creates a tag at `target` (`tag::create_tag`): lightweight, or annotated when `message` is `Some` (signing
    /// follows `tag.gpgSign` for annotated tags). An existing tag name is an error (standard dialog).
    /// Refresh: TAGS | LOG.
    pub fn create_tag(
        &mut self,
        name: String,
        target: ObjectId,
        message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let descriptor = OperationDescriptor {
            kind: OperationKind::CreateTag,
            title: format!("Create tag '{name}'"),
            detail: None,
            scope: Scope::RefsOnly,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::CreateTag, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            let opts = TagOptions {
                message,
                ..TagOptions::default()
            };
            git::tag::create_tag(repo, env.perm(), ctx, &name, target, &opts)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::TAGS | RefreshSet::LOG);
        self.run_op(spec, window, cx)
    }

    /// Deletes a tag locally (`tag::delete_tag`; deleting it on a remote is `delete_remote_tag`).
    /// Refresh: TAGS | LOG.
    pub fn delete_tag(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let descriptor = OperationDescriptor {
            kind: OperationKind::DeleteTag,
            title: format!("Delete tag '{name}'"),
            detail: None,
            scope: Scope::RefsOnly,
            untracked: UntrackedCapture::None,
        };
        let spec = OpSpec::new(OpKind::DeleteTag, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::tag::delete_tag(repo, env.perm(), ctx, &name)
        })
        .record(descriptor)
        .base_refresh(RefreshSet::TAGS | RefreshSet::LOG);
        self.run_op(spec, window, cx)
    }
}
