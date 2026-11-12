//! Remote operations of the catalogue (spec 21 §2.11; owned by G3): add, edit and delete. None of them is recorded
//! by the oplog (17 §4.4); the backend validates names and URLs without spawning Git.

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec};
use crate::backend::{RefreshSet, git};
use crate::tab::RepoTab;

impl RepoTab {
    /// Adds a remote (`remote::add_remote`); `push_url` is set only when it differs from `url`.
    /// Refresh: REMOTES | CONFIG.
    pub fn add_remote(
        &mut self,
        name: String,
        url: String,
        push_url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::AddRemote, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::remote::add_remote(repo, env.perm(), ctx, &name, &url, push_url.as_deref())
        })
        .base_refresh(RefreshSet::REMOTES | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Changes the URLs of a remote (`remote::set_remote_urls`; the name is immutable); `None` removes a separate
    /// push URL. Refresh: REMOTES | CONFIG.
    pub fn edit_remote(
        &mut self,
        name: String,
        url: String,
        push_url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::UpdateRemote, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::remote::set_remote_urls(repo, env.perm(), ctx, &name, &url, push_url.as_deref())
        })
        .base_refresh(RefreshSet::REMOTES | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Removes a remote together with its remote-tracking branches (`remote::remove_remote`). Gitnuro asks for no
    /// confirmation (09 §4.0). Refresh: REMOTES | BRANCHES | LOG | CONFIG.
    pub fn delete_remote(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::DeleteRemote, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::remote::remove_remote(repo, env.perm(), ctx, &name)
        })
        .base_refresh(
            RefreshSet::REMOTES | RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::CONFIG,
        );
        self.run_op(spec, window, cx)
    }
}
