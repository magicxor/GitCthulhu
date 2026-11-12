use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use futures::{FutureExt, select};

use super::executor::GitExecutor;
use crate::{
    RefSpec,
    askpass_harness::{
        AskpassError, AskpassSession, HarnessLocation, HostKeyPolicy,
        askpass_exe_next_to_current_exe, http_stall_limit_args, ssh_env,
    },
};

/// Higher level errors that can occur when interacting with the CLI.
///
/// You probably don't want to use this type. Use [`Error`] instead.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError<Eexec: std::error::Error + core::fmt::Debug + Send + Sync + 'static> {
    /// The git command could not be executed.
    #[error("failed to execute git command: {0}")]
    Exec(Eexec),
    /// The askpass machinery failed or the prompt was refused.
    #[error(transparent)]
    Askpass(#[from] AskpassError),
    /// The git command exited with a non-zero exit code.
    #[error(
        "git command exited with non-zero exit code {status}:

ARGS:
{args:?}

STDOUT:
{stdout}

STDERR:
{stderr}"
    )]
    Failed {
        /// The exit code.
        status: usize,
        /// The arguments.
        args: Vec<String>,
        /// Captured stdout.
        stdout: String,
        /// Captured stderr.
        stderr: String,
    },
    /// The remote configuration could not be read.
    #[error("failed to read configuration for remote `{remote}`: {source}")]
    RemoteConfiguration {
        /// The remote whose configuration could not be read.
        remote: String,
        /// The configuration lookup error.
        #[source]
        source: gix::Error,
    },
    /// The repository could not be opened.
    #[error("failed to open repository at `{}`: {source}", path.display())]
    RepositoryOpen {
        /// The repository path that could not be opened.
        path: PathBuf,
        /// The repository open error.
        #[source]
        source: gix::Error,
    },
}

/// Higher level errors that can occur when interacting with the CLI.
pub type Error<E> = RepositoryError<<E as GitExecutor>::Error>;

enum HarnessEnv<P: AsRef<Path>> {
    /// The contained P is the repository's worktree directory or its `.git` directory.
    Repo(P),
    /// The contained P is the path that the command should be executed in
    Global(P),
}

impl<P: AsRef<Path>> HarnessEnv<P> {
    fn location(&self) -> HarnessLocation<'_> {
        match self {
            HarnessEnv::Repo(p) => HarnessLocation::Repo(p.as_ref()),
            HarnessEnv::Global(_) => HarnessLocation::Global,
        }
    }

    fn cwd(&self) -> &Path {
        match self {
            HarnessEnv::Repo(p) | HarnessEnv::Global(p) => p.as_ref(),
        }
    }
}

#[cold]
async fn execute_with_auth_harness<P, F, Fut, E>(
    harness_env: HarnessEnv<P>,
    executor: &E,
    args: &[&str],
    on_prompt: Option<F>,
) -> Result<(usize, String, String), Error<E>>
where
    P: AsRef<Path>,
    E: GitExecutor,
    F: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let mut envs: HashMap<String, String> =
        ssh_env(harness_env.location(), HostKeyPolicy::AcceptNew)
            .into_iter()
            .collect();
    let stall_limits = http_stall_limit_args(harness_env.location());
    let args: Vec<&str> = stall_limits
        .iter()
        .map(String::as_str)
        .chain(args.iter().copied())
        .collect();

    let cwd = harness_env.cwd();
    let Some(on_prompt) = on_prompt else {
        // Directly execute the Git command without the askpass machinery. This is useful for the CLI, as the child
        // process simply inherits stdin from the parent process, and doesn't need the askpass helper.
        return executor
            .execute(&args, cwd, Some(envs))
            .await
            .map_err(Error::<E>::Exec);
    };

    let session = AskpassSession::new(askpass_exe_next_to_current_exe()?).await?;
    envs.extend(session.env());

    let mut child_process = core::pin::pin! {
        async {
            executor
                .execute(&args, cwd, Some(envs))
                .await
                .map_err(Error::<E>::Exec)
        }.fuse()
    };
    let mut serving = core::pin::pin!(session.serve(on_prompt).fuse());
    select! {
        res = child_process => res,
        res = serving => match res {
            Ok(never) => match never {},
            Err(err) => Err(err.into()),
        },
    }
}

/// Fetches from the given remote in the repository using its configured fetch refspecs.
///
/// If `on_prompt` is provided, we override SSH_ASKPASS to point to our custom askpass client to
/// ferry prompts and responses between the SSH process and this process. This should be used
/// carefully as it will unceremoniously override any askpass configuration the user might have,
/// making it unsuitable for use in e.g. the CLI. It is designed to be used with a GUI/TUI.
pub async fn fetch<P, F, Fut, E>(
    repo_path: P,
    executor: E,
    remote: &str,
    on_prompt: Option<F>,
) -> Result<(), crate::Error<Error<E>>>
where
    P: AsRef<Path>,
    E: GitExecutor,
    F: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let repo_path = repo_path.as_ref();
    let repo = gix::open(repo_path).map_err(|source| Error::<E>::RepositoryOpen {
        path: repo_path.to_owned(),
        source,
    })?;
    let refspecs = fetch_refspecs(&repo, remote).map_err(|source| {
        let remote_not_found = source.is_not_found();
        let source = Error::<E>::RemoteConfiguration {
            remote: remote.to_owned(),
            source,
        };
        if remote_not_found {
            crate::Error::NoSuchRemote(remote.to_owned(), source)
        } else {
            source.into()
        }
    })?;
    let mut args = vec!["fetch", "--quiet"];

    args.push(remote);
    args.extend(refspecs.iter().map(String::as_str));

    let (status, stdout, stderr) =
        execute_with_auth_harness(HarnessEnv::Repo(repo_path), &executor, &args, on_prompt).await?;

    if status == 0 {
        Ok(())
    } else {
        // Was the ref not found?
        if let Some(refname) = stderr
            .lines()
            .find(|line| line.to_lowercase().contains("couldn't find remote ref"))
            .map(|line| line.split_whitespace().last().unwrap_or_default())
        {
            Err(crate::Error::RefNotFound(refname.to_owned()))?
        } else if stderr.to_lowercase().contains("permission denied") {
            Err(crate::Error::AuthorizationFailed(Error::<E>::Failed {
                status,
                args: args.into_iter().map(Into::into).collect(),
                stdout,
                stderr,
            }))?
        } else {
            Err(Error::<E>::Failed {
                status,
                args: args.into_iter().map(Into::into).collect(),
                stdout,
                stderr,
            })?
        }
    }
}

/// Deletes the remote-tracking refs of `remote` whose branches are gone from it, like
/// `git remote prune`. Unlike a pruning fetch, it never brings new commits.
pub async fn prune<P, F, Fut, E>(
    repo_path: P,
    executor: E,
    remote: &str,
    on_prompt: Option<F>,
) -> Result<(), crate::Error<Error<E>>>
where
    P: AsRef<Path>,
    E: GitExecutor,
    F: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let repo_path = repo_path.as_ref();
    let args = ["remote", "prune", remote];
    let (status, stdout, stderr) =
        execute_with_auth_harness(HarnessEnv::Repo(repo_path), &executor, &args, on_prompt).await?;
    if status == 0 {
        Ok(())
    } else {
        Err(Error::<E>::Failed {
            status,
            args: args.into_iter().map(Into::into).collect(),
            stdout,
            stderr,
        })?
    }
}

fn fetch_refspecs(repo: &gix::Repository, remote: &str) -> gix::Result<Vec<String>> {
    let refspecs: Vec<_> = repo
        .find_remote(remote)?
        .refspecs(gix::remote::Direction::Fetch)
        .iter()
        .map(|spec| spec.to_ref().to_bstring().to_string())
        .collect();

    Ok(if refspecs.is_empty() {
        vec![format!("+refs/heads/*:refs/remotes/{remote}/*")]
    } else {
        refspecs
    })
}

/// Pushes a refspec to the given remote in the repository at the given path.
///
/// If `on_prompt` is provided, we override SSH_ASKPASS to point to our custom askpass client to
/// ferry prompts and responses between the SSH process and this process. This should be used
/// carefully as it will unceremoniously override any askpass configuration the user might have,
/// making it unsuitable for use in e.g. the CLI. It is designed to be used with a GUI/TUI.
#[expect(clippy::too_many_arguments)]
pub async fn push<P, F, Fut, E>(
    repo_path: P,
    executor: E,
    remote: &str,
    refspec: RefSpec,
    force: bool,
    force_push_protection: bool,
    on_prompt: Option<F>,
    push_opts: Vec<String>,
) -> Result<String, crate::Error<Error<E>>>
where
    P: AsRef<Path>,
    E: GitExecutor,
    F: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let mut args = vec!["push", "--quiet"];

    let is_deletion = refspec.source.is_none();
    let refspec = refspec.to_string();

    args.push(remote);
    args.push(&refspec);

    if force {
        if force_push_protection {
            args.push("--force-with-lease");
            // `--force-if-includes` verifies the remote tip was integrated into the local branch
            // being pushed; a deletion has no local side, so the check can never pass there and
            // the lease alone provides the compare-and-swap against our remote-tracking ref.
            if !is_deletion {
                args.push("--force-if-includes");
            }
        } else {
            args.push("--force");
        }
    }

    for opt in push_opts.iter() {
        args.push("-o");
        args.push(opt.as_str());
    }

    let (status, stdout, stderr) =
        execute_with_auth_harness(HarnessEnv::Repo(repo_path), &executor, &args, on_prompt).await?;

    if status == 0 {
        return Ok(stderr);
    }

    let base_error = Error::<E>::Failed {
        status,
        args: args.into_iter().map(Into::into).collect(),
        stdout,
        stderr: stderr.clone(),
    };

    if status == 1 && force && force_push_protection {
        return Err(crate::Error::ForcePushProtection(base_error));
    }

    // Check for specific error patterns in stderr
    if let Some(refname) = stderr
        .lines()
        .find(|line| line.to_lowercase().contains("does not match any"))
        .and_then(|line| line.split_whitespace().last())
    {
        return Err(crate::Error::RefNotFound(refname.to_owned()));
    }

    if stderr.to_lowercase().contains("permission denied") {
        return Err(crate::Error::AuthorizationFailed(base_error));
    }

    if stderr.to_lowercase().contains("(no new changes)") {
        return Err(crate::Error::GerritNoNewChanges(base_error));
    }

    // Detected last so the more specific typed cases above always win. A non-fast-forward
    // rejection is recoverable: the caller can re-fetch and retry.
    let lowercased_stderr = stderr.to_lowercase();
    if lowercased_stderr.contains("! [rejected]")
        || lowercased_stderr.contains("(non-fast-forward)")
        || lowercased_stderr.contains("(fetch first)")
    {
        return Err(crate::Error::NonFastForward(base_error));
    }

    Err(base_error.into())
}

/// Clones the given repository URL to the target directory.
///
/// If `on_prompt` is provided, we override SSH_ASKPASS to point to our custom askpass client to
/// ferry prompts and responses between the SSH process and this process. This should be used
/// carefully as it will unceremoniously override any askpass configuration the user might have,
/// making it unsuitable for use in e.g. the CLI. It is designed to be used with a GUI/TUI.
///
/// Unlike fetch/push, this function always uses the Git CLI regardless of any
/// backend selection, as it needs to work before a repository exists.
pub async fn clone<P, F, Fut, E>(
    repository_url: &str,
    target_dir: P,
    executor: E,
    on_prompt: Option<F>,
) -> Result<(), crate::Error<Error<E>>>
where
    P: AsRef<Path>,
    E: GitExecutor,
    F: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Option<String>> + Send + 'static,
{
    let target_dir = target_dir.as_ref();

    // For clone, we run from the parent directory of the target
    let work_dir = target_dir.parent().unwrap_or(Path::new("."));

    let target_dir_str = target_dir.to_string_lossy();
    let args = vec!["clone", "--", repository_url, &target_dir_str];

    let (status, stdout, stderr) =
        execute_with_auth_harness(HarnessEnv::Global(work_dir), &executor, &args, on_prompt)
            .await?;

    if status == 0 {
        Ok(())
    } else if stderr.to_lowercase().contains("permission denied") {
        Err(crate::Error::AuthorizationFailed(Error::<E>::Failed {
            status,
            args: args.into_iter().map(Into::into).collect(),
            stdout,
            stderr,
        }))?
    } else if stderr
        .to_lowercase()
        .contains("already exists and is not an empty directory")
    {
        Err(crate::Error::RemoteExists(
            target_dir.display().to_string(),
            Error::<E>::Failed {
                status,
                args: args.into_iter().map(Into::into).collect(),
                stdout,
                stderr,
            },
        ))?
    } else {
        Err(Error::<E>::Failed {
            status,
            args: args.into_iter().map(Into::into).collect(),
            stdout,
            stderr,
        })?
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fetch_distinguishes_missing_remotes_from_invalid_configuration() -> anyhow::Result<()>
    {
        let tmp = but_testsupport::gix_testtools::tempfile::tempdir()?;
        let repo = gix::init(tmp.path())?;
        let on_prompt = None::<fn(String) -> std::future::Ready<Option<String>>>;
        let err = super::fetch(tmp.path(), crate::tokio::TokioExecutor, "origin", on_prompt)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::Error::NoSuchRemote(..)),
            "a missing remote retains its public error variant"
        );

        but_core::git_config::edit_repo_config(&repo, gix::config::Source::Local, |config| {
            config.set_raw_value("remote.origin.url", "https://example.com/repo")?;
            config.set_raw_value(
                "remote.origin.fetch",
                "refs/heads/*:refs/remotes/origin/*:extra",
            )?;
            Ok(())
        })?;
        let err = super::fetch(tmp.path(), crate::tokio::TokioExecutor, "origin", on_prompt)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                crate::Error::Backend(super::RepositoryError::RemoteConfiguration { .. })
            ),
            "an existing remote with an invalid refspec remains a configuration error"
        );
        Ok(())
    }
}
