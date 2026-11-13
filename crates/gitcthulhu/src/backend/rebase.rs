//! Interactive-rebase reads (owned by G9 after G0b; spec 21 §2.4).

use super::{GitError, ObjectId, RebasePlan, Repo};

/// The editable plan of an interactive rebase onto `base` (`None` = the root).
pub fn plan_interactive(repo: &Repo, base: Option<ObjectId>) -> Result<RebasePlan, GitError> {
    gitcthulhu_git::rebase::plan_interactive(repo, base)
}
