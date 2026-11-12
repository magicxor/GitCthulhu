//! AI backend calls (owned by X3 after G0b; spec 21 §2.10, §5.3). Milestone 1 ships the diff input and the status
//! query; X3 adds configuration and generation here. Service calls are behind `cfg(feature = "services")`.

use super::{CancelToken, GitError, PatchText, PatchTextOptions, Repo};

/// The staged changes as budgeted patch text, the input of commit-message generation.
pub fn staged_patch_text(repo: &Repo, cancel: &CancelToken) -> Result<PatchText, GitError> {
    gitcthulhu_git::diff::staged_patch_text(repo, &PatchTextOptions::default(), cancel)
}

/// Whether the AI service can generate text now (`AiService::status` is `Ready`).
#[cfg(feature = "services")]
pub fn is_ready(services: &super::services::ServicesHandle) -> bool {
    matches!(
        services.services().ai.status(),
        gitcthulhu_services::ai::AiStatus::Ready { .. }
    )
}
