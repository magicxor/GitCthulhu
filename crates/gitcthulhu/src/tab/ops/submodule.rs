//! Submodule operations of the catalogue (spec 21 §2.11; owned by G3). Not recorded by the oplog (17 §4.4). Add,
//! init and update talk to the network and are cancellable; sync, deinit and delete are local.

use gpui_kit::{Context, Window};

use super::{Cancellable, OpKind, OpSpec};
use crate::backend::{RefreshSet, RelaPath, git};
use crate::tab::RepoTab;

impl RepoTab {
    /// Adds a submodule (`submodule::add`; the path must not exist or be empty). Cancellable.
    /// Refresh: SUBMODULES | STATUS | CONFIG.
    pub fn add_submodule(
        &mut self,
        url: String,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::AddSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::add(repo, env.perm(), ctx, &url, &path, None, None)
        })
        .cancellable(Cancellable::Yes)
        .base_refresh(RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Initialises and updates a submodule (`submodule::init_update`, not recursive). Cancellable.
    /// Refresh: SUBMODULES | STATUS.
    pub fn init_submodule(
        &mut self,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::InitSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::init_update(repo, env.perm(), ctx, std::slice::from_ref(&path), false)
        })
        .cancellable(Cancellable::Yes)
        .base_refresh(RefreshSet::SUBMODULES | RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Updates a submodule to the commit the index records (`submodule::update`). Cancellable.
    /// Refresh: SUBMODULES | STATUS.
    pub fn update_submodule(
        &mut self,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::UpdateSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::update(repo, env.perm(), ctx, std::slice::from_ref(&path), false)
        })
        .cancellable(Cancellable::Yes)
        .base_refresh(RefreshSet::SUBMODULES | RefreshSet::STATUS);
        self.run_op(spec, window, cx)
    }

    /// Copies the URL of `.gitmodules` into the configuration (`submodule::sync`).
    /// Refresh: SUBMODULES | CONFIG.
    pub fn sync_submodule(
        &mut self,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::SyncSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::sync(repo, env.perm(), ctx, std::slice::from_ref(&path), false)
        })
        .base_refresh(RefreshSet::SUBMODULES | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Deinitialises a submodule: its working directory is emptied, `.gitmodules` and the index entry stay
    /// (`submodule::deinit`, no force, so local changes make Git refuse). Gitnuro defines but never wires it; the
    /// side-panel menu does not offer it either.
    /// Refresh: SUBMODULES | STATUS | CONFIG.
    pub fn deinit_submodule(
        &mut self,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::DeinitSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::deinit(repo, env.perm(), ctx, std::slice::from_ref(&path), false)
        })
        .base_refresh(RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Removes a submodule completely (`submodule::remove`: deinit, `git rm`, the cloned repository in
    /// `.git/modules`). The menu asks first because local changes inside the submodule are lost.
    /// Refresh: SUBMODULES | STATUS | CONFIG | LOG.
    pub fn delete_submodule(
        &mut self,
        path: RelaPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::DeleteSubmodule, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::submodule::remove(repo, env.perm(), ctx, &path)
        })
        .base_refresh(
            RefreshSet::SUBMODULES | RefreshSet::STATUS | RefreshSet::CONFIG | RefreshSet::LOG,
        );
        self.run_op(spec, window, cx)
    }
}
