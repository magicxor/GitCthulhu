//! "Checkout pull request" (spec 21 §2.11 note, 20 §8.3): the forge plan comes from the backend (the review from the
//! cache, else from the forge: a background call), the checkout itself is one recorded tab operation: add the
//! remote if it is new, fetch the plan's refspec, create the branch at the fetched commit, check it out and set the
//! upstream when the fetched ref is a remote-tracking branch.

use std::rc::Rc;

use gpui_kit::{App, Context, Window};

use crate::backend::forge::{CheckoutPlanInfo, RepoHandle};
use crate::backend::present::OpFailure;
use crate::backend::{
    GitError, OperationDescriptor, OperationKind, RefreshSet, Scope, UntrackedCapture,
};
use crate::notifications::ToastKind;
use crate::tab::ops::{Cancellable, Handled, OpKind, OpSpec};
use crate::tab::{RepoTab, TabCtx};

/// What a checkout can change (20 §2.6): everything but the config, plus the remote it may add.
const REFRESH: RefreshSet = RefreshSet::STATUS
    .union(RefreshSet::LOG)
    .union(RefreshSet::BRANCHES)
    .union(RefreshSet::REMOTES)
    .union(RefreshSet::REPO_STATE)
    .union(RefreshSet::SUBMODULES)
    .union(RefreshSet::WORKTREES)
    .union(RefreshSet::CONFIG);

/// Looks the plan of pull request `number` up (in the background) and runs the checkout on the tab.
pub fn start(tab: &TabCtx, handle: RepoHandle, number: i64, window: &mut Window, cx: &mut App) {
    let plan = crate::backend::bg(cx, move || handle.checkout_plan(number));
    let tab = tab.clone();
    window
        .spawn(cx, async move |cx| {
            let plan = plan.await;
            cx.update(|window, cx| match plan {
                Ok(plan) => tab.with_tab(window, cx, |tab, window, cx| {
                    tab.check_out_pull_request(plan, number, window, cx);
                }),
                Err(failure) => crate::app_view::show_toast(
                    cx,
                    Some(tab.id),
                    ToastKind::Error,
                    format!(
                        "Cannot check out pull request #{number}: {}",
                        failure.message
                    ),
                    None,
                ),
            })
            .ok();
        })
        .detach();
}

impl RepoTab {
    /// Checks the pull request `number` out by `plan` (see the module documentation). A local branch of the planned
    /// name that exists already asks for another name. Every error uses the standard presentation otherwise.
    pub fn check_out_pull_request(
        &mut self,
        plan: CheckoutPlanInfo,
        number: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let local = plan.local_branch.clone();
        let descriptor = OperationDescriptor {
            kind: OperationKind::CreateBranchAndCheckout,
            title: format!("Check out pull request #{number} as '{local}'"),
            detail: None,
            scope: Scope::Full,
            untracked: UntrackedCapture::None,
        };
        let retry_plan = plan.clone();
        let spec = OpSpec::new(OpKind::CheckoutRemoteBranch, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            crate::backend::forge::checkout_pull_request(repo, env.perm(), ctx, &plan)
        })
        .record(descriptor)
        .base_refresh(REFRESH)
        .cancellable(Cancellable::WithWarning)
        .label(format!("Fetch pull request #{number}"))
        .toast(format!("Checked out pull request #{number} as '{local}'"))
        .on_error(move |tab, failure, window, cx| {
            let collided = matches!(
                failure,
                OpFailure::Git(err) if matches!(&**err, GitError::AlreadyExists { what: "branch", .. })
            );
            if !collided {
                return Handled::No;
            }
            let suggested = format!("{}-pr-{number}", retry_plan.local_branch);
            let ctx = tab.ctx.clone();
            let plan = retry_plan.clone();
            crate::dialogs::branch::choose_local_name(
                window,
                cx,
                suggested,
                Rc::new(move |name: String, window: &mut Window, cx: &mut App| {
                    let plan = CheckoutPlanInfo {
                        local_branch: name,
                        ..plan.clone()
                    };
                    ctx.with_tab(window, cx, move |tab, window, cx| {
                        tab.check_out_pull_request(plan, number, window, cx);
                    });
                }),
            );
            Handled::Yes
        });
        self.run_op(spec, window, cx)
    }
}
