//! Branch operations of the catalogue (spec 21 §2.11, §2.9; owned by G3): checkout of branches, remote branches,
//! commits and tags, create, rename, delete and the upstream setting.
//!
//! Every method builds an `OpSpec` after the reference patterns in `files.rs` and `stash.rs` and calls `run_op`.
//! Oplog titles are the English texts of 17 §4.2; every checkout is a full snapshot (index and worktree change),
//! the ref-only operations are `Scope::RefsOnly`.

use std::rc::Rc;

use gpui_kit::{App, Context, Window};

use super::{Handled, OpKind, OpSpec};
use crate::backend::present::OpFailure;
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{
    ByteSlice as _, CheckoutOptions, FullName, GitError, ObjectId, OperationDescriptor,
    OperationKind, RefreshSet, Scope, StartPoint, UntrackedCapture, git,
};
use crate::tab::RepoTab;

/// What a checkout can change (20 §2.6): everything but the config.
const CHECKOUT_REFRESH: RefreshSet = RefreshSet::STATUS
    .union(RefreshSet::LOG)
    .union(RefreshSet::BRANCHES)
    .union(RefreshSet::REPO_STATE)
    .union(RefreshSet::SUBMODULES)
    .union(RefreshSet::WORKTREES);

/// The short name of a ref (`feature/x`, `origin/main`).
pub(crate) fn short_name(full: &FullName) -> String {
    full.shorten().to_str_lossy().into_owned()
}

fn descriptor(kind: OperationKind, title: String, scope: Scope) -> OperationDescriptor {
    OperationDescriptor {
        kind,
        title,
        detail: None,
        scope,
        untracked: UntrackedCapture::None,
    }
}

/// The 7-digit id of a commit for oplog titles.
fn short_id(id: ObjectId) -> String {
    id.to_hex_with_len(7).to_string()
}

/// A local name that is free to try after `taken` collided: "origin/feat/x" -> "origin-feat/x"; a name chosen by
/// the user gets "-2".
fn suggest_local_name(remote_ref: &FullName, taken: Option<&str>) -> String {
    match taken {
        Some(name) => format!("{name}-2"),
        None => short_name(remote_ref).replacen('/', "-", 1),
    }
}

impl RepoTab {
    /// Checks out a local branch (`branch::checkout_branch`). Errors that Git explains (local changes that would
    /// be overwritten, branch checked out in another worktree) use the standard presentation.
    /// Refresh: STATUS | LOG | BRANCHES | REPO_STATE | SUBMODULES | WORKTREES.
    pub fn checkout_branch(
        &mut self,
        full: FullName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Checkout '{}'", short_name(&full));
        let spec = OpSpec::new(OpKind::CheckoutBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::checkout_branch(repo, env.perm(), ctx, &full, CheckoutOptions::default())
        })
        .record(descriptor(OperationKind::Checkout, title, Scope::Full))
        .base_refresh(CHECKOUT_REFRESH);
        self.run_op(spec, window, cx)
    }

    /// Checks out a remote branch as a tracking branch (`branch::checkout_remote_branch`); `local_name` defaults to
    /// the branch part of the remote ref. When a different local branch of that name exists, the "Choose another
    /// local name" dialog asks for one and the checkout is retried (21 §2.9).
    /// Refresh: the checkout set | CONFIG.
    pub fn checkout_remote_branch(
        &mut self,
        remote_ref: FullName,
        local_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shown = local_name.clone().unwrap_or_else(|| {
            let remote = short_name(&remote_ref);
            remote
                .split_once('/')
                .map_or(remote.clone(), |(_, branch)| branch.to_owned())
        });
        let title = format!("Checkout '{shown}'");
        let (run_ref, run_name) = (remote_ref.clone(), local_name.clone());
        let spec = OpSpec::new(OpKind::CheckoutRemoteBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::checkout_remote_branch(
                repo,
                env.perm(),
                ctx,
                &run_ref,
                run_name.as_deref(),
                CheckoutOptions::default(),
            )
        })
        .record(descriptor(
            OperationKind::CheckoutRemoteBranch,
            title,
            Scope::Full,
        ))
        .base_refresh(CHECKOUT_REFRESH | RefreshSet::CONFIG)
        .on_error(move |tab, failure, window, cx| {
            let collided = matches!(
                failure,
                OpFailure::Git(err) if matches!(
                    &**err,
                    GitError::AlreadyExists { what: "branch", .. }
                )
            );
            if !collided {
                return Handled::No;
            }
            let suggested = suggest_local_name(&remote_ref, local_name.as_deref());
            let ctx = tab.ctx.clone();
            crate::dialogs::branch::choose_local_name(
                window,
                cx,
                suggested,
                Rc::new(move |name: String, window: &mut Window, cx: &mut App| {
                    let remote_ref = remote_ref.clone();
                    ctx.with_tab(window, cx, move |tab, window, cx| {
                        tab.checkout_remote_branch(remote_ref, Some(name), window, cx);
                    });
                }),
            );
            Handled::Yes
        });
        self.run_op(spec, window, cx)
    }

    /// Checks out a commit with a detached HEAD (`branch::checkout_detached`).
    /// Refresh: the checkout set.
    pub fn checkout_commit(
        &mut self,
        id: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.checkout_detached(OpKind::CheckoutCommit, id, window, cx)
    }

    /// Checks out the commit a tag points to with a detached HEAD (`branch::checkout_detached`).
    /// Refresh: the checkout set.
    pub fn checkout_tag(
        &mut self,
        _full: FullName,
        target: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.checkout_detached(OpKind::CheckoutTag, target, window, cx)
    }

    fn checkout_detached(
        &mut self,
        kind: OpKind,
        id: ObjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Checkout {}", short_id(id));
        let spec = OpSpec::new(kind, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::checkout_detached(repo, env.perm(), ctx, id, CheckoutOptions::default())
        })
        .record(descriptor(
            OperationKind::CheckoutCommit,
            title,
            Scope::Full,
        ))
        .base_refresh(CHECKOUT_REFRESH);
        self.run_op(spec, window, cx)
    }

    /// Creates a branch at `start`, optionally checking it out (`branch::create_branch`; the name is validated by
    /// the backend without spawning Git). Refresh: with checkout the checkout set | CONFIG, else BRANCHES | LOG.
    pub fn create_branch(
        &mut self,
        name: String,
        start: StartPoint,
        checkout: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Create branch '{name}'");
        let (kind, scope, refresh) = if checkout {
            (
                OperationKind::CreateBranchAndCheckout,
                Scope::Full,
                CHECKOUT_REFRESH | RefreshSet::CONFIG,
            )
        } else {
            (
                OperationKind::CreateBranch,
                Scope::RefsOnly,
                RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::CONFIG,
            )
        };
        let spec = OpSpec::new(OpKind::CreateBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::create_branch(repo, env.perm(), ctx, &name, &start, checkout)
        })
        .record(descriptor(kind, title, scope))
        .base_refresh(refresh);
        self.run_op(spec, window, cx)
    }

    /// Renames a local branch (`branch::rename_branch`; its configuration moves with it).
    /// Refresh: BRANCHES | LOG | WORKTREES | CONFIG.
    pub fn rename_branch(
        &mut self,
        full: FullName,
        new_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Rename branch '{}' to '{new_name}'", short_name(&full));
        let spec = OpSpec::new(OpKind::RenameBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::rename_branch(repo, env.perm(), ctx, &full, &new_name)
        })
        .record(descriptor(
            OperationKind::RenameBranch,
            title,
            Scope::RefsOnly,
        ))
        .base_refresh(
            RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::WORKTREES | RefreshSet::CONFIG,
        );
        self.run_op(spec, window, cx)
    }

    /// Deletes a local branch (`branch::delete_branch`): safe deletion unless `force`. A branch that is not fully
    /// merged asks "Branch 'x' is not fully merged. Delete it anyway?" and is deleted with `force` on yes
    /// (21 §2.9). Refresh: BRANCHES | LOG (| CONFIG).
    pub fn delete_branch(
        &mut self,
        full: FullName,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = format!("Delete branch '{}'", short_name(&full));
        let run_full = full.clone();
        let spec = OpSpec::new(OpKind::DeleteBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::delete_branch(repo, env.perm(), ctx, &run_full, force)
        })
        .record(descriptor(
            OperationKind::DeleteBranch,
            title,
            Scope::RefsOnly,
        ))
        .base_refresh(RefreshSet::BRANCHES | RefreshSet::LOG | RefreshSet::CONFIG)
        .on_error(move |tab, failure, window, cx| {
            let unmerged = !force
                && matches!(
                    failure,
                    OpFailure::Git(err) if matches!(&**err, GitError::NotFullyMerged { .. })
                );
            if !unmerged {
                return Handled::No;
            }
            let mut request = ConfirmRequest::new(
                "Delete branch",
                format!(
                    "Branch '{}' is not fully merged. Delete it anyway?",
                    short_name(&full)
                ),
            );
            request.confirm_label = "Delete".into();
            request.destructive = true;
            let ctx = tab.ctx.clone();
            crate::dialogs::confirm::open_with(
                window,
                cx,
                request,
                Box::new(move |yes, window, cx| {
                    if yes {
                        ctx.with_tab(window, cx, move |tab, window, cx| {
                            tab.delete_branch(full, true, window, cx);
                        });
                    }
                }),
            );
            Handled::Yes
        });
        self.run_op(spec, window, cx)
    }

    /// Sets (`Some`) or unsets (`None`) the upstream of a branch (`branch::set_upstream`).
    /// Refresh: BRANCHES | CONFIG.
    pub fn set_upstream(
        &mut self,
        full: FullName,
        upstream: Option<FullName>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::ChangeUpstream, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::branch::set_upstream(repo, env.perm(), ctx, &full, upstream.as_ref())
        })
        .base_refresh(RefreshSet::BRANCHES | RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }
}
