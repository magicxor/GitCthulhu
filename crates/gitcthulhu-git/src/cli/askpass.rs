//! Binding of an askpass session to the executor and the credential prompter (WP-B0c).
//!
//! Seam used by `cli::run` (WP-B0b). WP-B0c replaces the bodies with the `gitbutler_git::askpass_harness`
//! integration (§1.8, §2.3 step 3, §6) and keeps the signatures. Until then no prompt can be answered: Git fails by
//! itself ("terminal prompts disabled") and the default `GIT_SSH_COMMAND` / stall limits are not applied.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{HostKeyPolicy, OpContext, Result};

/// Environment and `-c` arguments every Mutation and Network command gets from the harness (§1.7): the default
/// `GIT_SSH_COMMAND` unless the user configured ssh, and `-c http.lowSpeedLimit=1 -c http.lowSpeedTime=60` for keys the
/// user has not set. `repo_dir` is where configuration is looked up (`None`: global configuration only). Blocking.
pub(super) fn transport_settings(
    repo_dir: Option<&Path>,
    host_key_policy: HostKeyPolicy,
) -> (Vec<(OsString, OsString)>, Vec<OsString>) {
    let _ = (repo_dir, host_key_policy);
    (Vec::new(), Vec::new())
}

/// One askpass server for the lifetime of one git invocation, answering prompts with `OpContext::prompter`.
pub(super) struct Session {
    _private: (),
}

impl Session {
    /// Start the server for `askpass_exe`. Errors are askpass infrastructure failures (`GitError::Other`).
    pub(super) async fn start(
        askpass_exe: &Path,
        ctx: &OpContext,
        repo: Option<PathBuf>,
    ) -> Result<Session> {
        let _ = (askpass_exe, ctx, repo);
        Ok(Session { _private: () })
    }

    /// Environment for the git child (`SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE=force`, pipe name, secret).
    pub(super) fn env(&self) -> Vec<(OsString, OsString)> {
        Vec::new()
    }

    /// Serve prompts. Completes only when a prompt was refused (`Ok(prompt)`: the prompter answered `Cancel`) or the
    /// server failed (`Err`); the executor keeps waiting for Git in both cases.
    pub(super) async fn serve(&self) -> Result<String> {
        std::future::pending().await
    }
}
