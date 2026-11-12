//! Reading and writing Git configuration (WP-B5).
//!
//! Reads come from the `gix` configuration snapshot of the open repository (plus the individual files for the
//! per-scope identities). Writes edit the configuration files in-process (`but_core::git_config`) and reload the
//! repository handle; the only `git` processes are `config --global --add safe.directory` and
//! `commit-graph write`.

use std::path::{Path, PathBuf};

use bstr::ByteSlice as _;
use gix::config::Source;

use crate::{
    CancelToken, CommandLog, ConfigScope, GitError, GitRuntime, Identity, IdentityInfo, OpClass,
    OpContext, OpOutcome, OpWarning, PartialIdentity, PullMode, RefreshSet, Repo, RepoExclusive,
    Result, SigningConfig, SigningFormat,
    cli::{CommandKind, GitCommand, GitOutput, run_checked, run_checked_global},
};

/// Wrap any displayable error into [`GitError::Other`].
pub(crate) fn other(err: impl std::fmt::Display) -> GitError {
    GitError::Other(anyhow::anyhow!("{err:#}"))
}

/// Collects the logs and warnings of the `git` commands one operation ran.
#[derive(Default)]
pub(crate) struct OpAccumulator {
    log: CommandLog,
    warnings: Vec<OpWarning>,
}

impl OpAccumulator {
    /// Record one finished command.
    pub(crate) fn push(&mut self, out: GitOutput) {
        self.log.commands.push(out.record);
        self.log.hooks.extend(out.hooks);
        self.log.removed_stale_locks.extend(out.removed_stale_locks);
        self.warnings.extend(out.warnings);
    }

    /// The finished outcome.
    pub(crate) fn finish<T>(self, value: T, refresh: RefreshSet) -> OpOutcome<T> {
        OpOutcome {
            value,
            refresh,
            log: self.log,
            warnings: self.warnings,
        }
    }
}

/// Run `cmd` and collect its output into a fresh accumulator.
pub(crate) fn run_one(
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<OpAccumulator> {
    let mut acc = OpAccumulator::default();
    acc.push(run_checked(repo, cmd, ctx, class)?);
    Ok(acc)
}

// ───────────────────────────── identity ─────────────────────────────

fn partial_from(file: &gix::config::File, keep: impl Fn(Source) -> bool) -> PartialIdentity {
    let mut out = PartialIdentity::default();
    if let Some(sections) = file.sections_by_name("user") {
        for section in sections {
            if section.header().subsection_name().is_some() || !keep(section.meta().source) {
                continue;
            }
            let body = section.body();
            if let Some(name) = body.value("name") {
                out.name = Some(name.to_str_lossy().into_owned());
            }
            if let Some(email) = body.value("email") {
                out.email = Some(email.to_str_lossy().into_owned());
            }
        }
    }
    out
}

/// `later` fields win over `earlier` ones.
fn overlay(earlier: PartialIdentity, later: &PartialIdentity) -> PartialIdentity {
    PartialIdentity {
        name: later.name.clone().or(earlier.name),
        email: later.email.clone().or(earlier.email),
    }
}

fn is_global_source(source: Source) -> bool {
    matches!(source, Source::User | Source::Git)
}

fn is_system_source(source: Source) -> bool {
    matches!(source, Source::System | Source::GitInstallation)
}

/// The global (user-level) configuration file of `rt`: `GIT_CONFIG_GLOBAL` of a test runtime, else `~/.gitconfig`.
fn global_config_path(rt: &GitRuntime) -> Option<PathBuf> {
    match rt.test_isolation() {
        Some(isolation) => Some(isolation.global_config.clone()),
        None => gix::config_path(Source::User, &gix::open::Options::default()).ok(),
    }
}

/// The identities of the global and system configuration.
fn global_and_system(repo: Option<&Repo>) -> Result<(PartialIdentity, PartialIdentity)> {
    if let Some(repo) = repo.filter(|r| r.runtime().test_isolation().is_some()) {
        let global = global_config_path(repo.runtime())
            .filter(|path| path.is_file())
            .and_then(|path| gix::config::File::from_path_no_includes(path, Source::User).ok())
            .map(|file| partial_from(&file, |_| true))
            .unwrap_or_default();
        return Ok((global, PartialIdentity::default()));
    }
    let globals = gix::config::File::from_globals().map_err(other)?;
    Ok((
        partial_from(&globals, is_global_source),
        partial_from(&globals, is_system_source),
    ))
}

/// The identity from the repository's resolved configuration (every scope, `include` and `includeIf` followed), as
/// `git commit` sees it. An isolated test runtime reads no global file and injects its overrides as `Api` sections
/// (which would beat the local file): those are skipped and the isolated global file is layered underneath instead.
fn resolved_identity(repo: &Repo) -> PartialIdentity {
    let gix_repo = repo.local();
    let snapshot = gix_repo.config_snapshot();
    let isolated = repo.runtime().test_isolation().is_some();
    let resolved = partial_from(snapshot.plumbing(), |source| {
        !(isolated && source == Source::Api)
    });
    if isolated {
        let (global, _) = global_and_system(Some(repo)).unwrap_or_default();
        overlay(global, &resolved)
    } else {
        resolved
    }
}

/// The configured identities and the one `git commit` would use.
///
/// `global` and `local` are what the individual files say (without includes); `effective` comes from the fully
/// resolved configuration (so an identity set through `includeIf` is honoured), then `GIT_AUTHOR_NAME` /
/// `GIT_AUTHOR_EMAIL`.
pub fn identity(repo: Option<&Repo>) -> Result<IdentityInfo> {
    let (global, system) = global_and_system(repo)?;
    let local = match repo {
        Some(repo) => {
            let gix_repo = repo.local();
            let local_file = but_core::git_config::open_repo_local_config_for_reading(&gix_repo)
                .map_err(other)?;
            let local = partial_from(&local_file, |_| true);
            // Per-worktree configuration overrides the repository's.
            let worktree = gix_repo
                .config_path(Source::Worktree)
                .ok()
                .filter(|path| path.is_file())
                .and_then(|path| {
                    gix::config::File::from_path_no_includes(path, Source::Worktree).ok()
                })
                .map(|file| partial_from(&file, |_| true))
                .unwrap_or_default();
            overlay(local, &worktree)
        }
        None => PartialIdentity::default(),
    };
    let env = PartialIdentity {
        name: non_empty_env("GIT_AUTHOR_NAME"),
        email: non_empty_env("GIT_AUTHOR_EMAIL"),
    };
    let configured = match repo {
        Some(repo) => resolved_identity(repo),
        // system < global
        None => overlay(system, &global),
    };
    // configuration < environment
    let merged = overlay(configured, &env);
    let effective = match (merged.name, merged.email) {
        (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => {
            Some(Identity { name, email })
        }
        _ => None,
    };
    Ok(IdentityInfo {
        global,
        local,
        effective,
    })
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

// ───────────────────────────── writing ─────────────────────────────

/// One edit of a configuration value.
type Edit<'a> = (&'a str, Option<&'a str>);

fn apply_edits(file: &mut gix::config::File, edits: &[Edit<'_>]) -> anyhow::Result<()> {
    for (key, value) in edits {
        match value {
            Some(value) => but_core::git_config::set_config_value(file, key, value)?,
            None => but_core::git_config::remove_config_value(file, key)?,
        }
    }
    Ok(())
}

fn edit_scope(repo: Option<&Repo>, scope: ConfigScope, edits: &[Edit<'_>]) -> Result<()> {
    for (key, _) in edits {
        validate_key(key)?;
    }
    match (scope, repo) {
        (ConfigScope::Local | ConfigScope::Worktree, None) => Err(GitError::Unsupported {
            what: "repository configuration without a repository",
        }),
        (ConfigScope::Local, Some(repo)) => {
            but_core::git_config::edit_repo_config(&repo.local(), Source::Local, |file| {
                apply_edits(file, edits)
            })
            .map(drop)
            .map_err(other)
        }
        (ConfigScope::Worktree, Some(repo)) => {
            let gix_repo = repo.local();
            // Git only reads `config.worktree` when this extension is on.
            but_core::git_config::edit_repo_config(&gix_repo, Source::Local, |file| {
                but_core::git_config::set_config_value(file, "extensions.worktreeConfig", "true")
            })
            .map_err(other)?;
            but_core::git_config::edit_repo_config(&gix_repo, Source::Worktree, |file| {
                apply_edits(file, edits)
            })
            .map(drop)
            .map_err(other)
        }
        (ConfigScope::Global, repo) => edit_global(repo, edits),
    }
}

fn edit_global(repo: Option<&Repo>, edits: &[Edit<'_>]) -> Result<()> {
    let isolated = repo.and_then(|repo| {
        repo.runtime()
            .test_isolation()
            .map(|iso| &iso.global_config)
    });
    match isolated {
        Some(path) => {
            let mut file = if path.is_file() {
                gix::config::File::from_path_no_includes(path.clone(), Source::User)
                    .map_err(other)?
            } else {
                gix::config::File::new(gix::config::file::Metadata::from(Source::User))
            };
            let before = file.to_bstring();
            apply_edits(&mut file, edits).map_err(other)?;
            if file.to_bstring() != before {
                std::fs::write(path, file.to_bstring().as_slice()).map_err(|source| {
                    GitError::Io {
                        path: path.clone(),
                        source,
                    }
                })?;
            }
            Ok(())
        }
        None => {
            but_core::git_config::edit_config(None, Source::User, |file| apply_edits(file, edits))
                .map(drop)
                .map_err(other)
        }
    }
}

fn validate_key(key: &str) -> Result<()> {
    use gix::config::AsKey as _;
    if key.is_empty() || key.trim() != key || key.try_as_key().is_none() {
        return Err(GitError::InvalidName {
            what: "config key",
            name: key.to_owned(),
        });
    }
    Ok(())
}

fn finish_config_write(repo: Option<&Repo>) -> Result<RefreshSet> {
    if let Some(repo) = repo {
        repo.reload()?;
    }
    Ok(RefreshSet::CONFIG)
}

/// Set or (with `None`) unset `user.name` / `user.email` in `scope`.
pub fn set_identity(
    repo: Option<&Repo>,
    scope: ConfigScope,
    identity: Option<&Identity>,
) -> Result<RefreshSet> {
    let edits = [
        ("user.name", identity.map(|i| i.name.as_str())),
        ("user.email", identity.map(|i| i.email.as_str())),
    ];
    edit_scope(repo, scope, &edits)?;
    finish_config_write(repo)
}

/// A configuration value.
pub fn get(repo: &Repo, key: &str) -> Result<Option<String>> {
    validate_key(key)?;
    Ok(repo
        .local()
        .config_snapshot()
        .string(key)
        .map(|value| value.to_str_lossy().into_owned()))
}

/// A boolean configuration value.
pub fn get_bool(repo: &Repo, key: &str) -> Result<Option<bool>> {
    validate_key(key)?;
    repo.local()
        .config_snapshot()
        .try_boolean(key)
        .map_err(other)
}

/// Set a configuration value; `None` => unset.
pub fn set(
    repo: Option<&Repo>,
    scope: ConfigScope,
    key: &str,
    value: Option<&str>,
) -> Result<RefreshSet> {
    edit_scope(repo, scope, &[(key, value)])?;
    finish_config_write(repo)
}

/// The effective signing configuration.
pub fn signing(repo: &Repo) -> Result<SigningConfig> {
    let gix_repo = repo.local();
    let snapshot = gix_repo.config_snapshot();
    let string = |key: &str| {
        snapshot
            .string(key)
            .map(|v| v.to_str_lossy().into_owned())
            .filter(|v| !v.is_empty())
    };
    let boolean = |key: &str| snapshot.try_boolean(key).ok().flatten().unwrap_or(false);
    let format = match string("gpg.format")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("ssh") => SigningFormat::Ssh,
        Some("x509") => SigningFormat::X509,
        _ => SigningFormat::OpenPgp,
    };
    let program = match format {
        SigningFormat::Ssh => string("gpg.ssh.program"),
        SigningFormat::X509 => string("gpg.x509.program"),
        SigningFormat::OpenPgp => string("gpg.openpgp.program"),
    }
    .or_else(|| string("gpg.program"));
    Ok(SigningConfig {
        commit_sign: boolean("commit.gpgsign"),
        tag_sign: boolean("tag.gpgsign"),
        format,
        key: string("user.signingkey"),
        program,
    })
}

/// `Some(true)` for every spelling of "rebase" (`true`, `merges`, `interactive`, ...), `Some(false)` for "no".
fn parse_rebase(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "merges" | "m" | "interactive" | "i" | "preserve" | "p" => {
            Some(true)
        }
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => None,
    }
}

/// `pull.rebase` / `pull.ff` / `branch.<b>.rebase`.
///
/// [`PullMode::FromConfig`] means "nothing is configured": Git would refuse a divergent pull, so the GUI has to ask.
pub fn pull_mode_default(repo: &Repo) -> Result<PullMode> {
    let gix_repo = repo.local();
    let snapshot = gix_repo.config_snapshot();
    let string = |key: &str| snapshot.string(key).map(|v| v.to_str_lossy().into_owned());
    let branch_rebase = gix_repo
        .head_name()
        .ok()
        .flatten()
        .and_then(|name| string(&format!("branch.{}.rebase", name.shorten())))
        .and_then(|v| parse_rebase(&v));
    let rebase = branch_rebase.or_else(|| string("pull.rebase").and_then(|v| parse_rebase(&v)));
    let ff = string("pull.ff").map(|v| v.trim().to_ascii_lowercase());
    Ok(match (rebase, ff.as_deref()) {
        (Some(true), _) => PullMode::Rebase,
        (_, Some("only")) => PullMode::FfOnly,
        (Some(false), _) | (None, Some(_)) => PullMode::Merge,
        (None, None) => PullMode::FromConfig,
    })
}

/// `git config --global --add safe.directory <path>` (UnsafeRepository recovery).
pub fn add_safe_directory(rt: &GitRuntime, path: &Path) -> Result<()> {
    let text = path.to_string_lossy();
    // Verbatim prefixes (from `canonicalize`) are not understood by Git: `\\?\UNC\srv\share` => `//srv/share`,
    // `\\?\C:\x` => `C:/x`.
    let text = match text.strip_prefix(r"\\?\UNC\") {
        Some(unc) => format!(r"\\{unc}"),
        None => text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned(),
    }
    .replace('\\', "/");
    let cmd = GitCommand::global(std::env::temp_dir(), CommandKind::Mutation)
        .args(["config", "--global", "--add", "safe.directory"])
        .arg(text);
    let ctx = OpContext::silent(CancelToken::new());
    run_checked_global(rt, cmd, &ctx, OpClass::Config).map(drop)
}

/// Whether a commit-graph file exists.
pub fn commit_graph_present(repo: &Repo) -> bool {
    let info = repo.common_dir().join("objects").join("info");
    info.join("commit-graph").is_file()
        || info
            .join("commit-graphs")
            .join("commit-graph-chain")
            .is_file()
}

/// `git commit-graph write --reachable --changed-paths` (offered once for large repos, §9).
pub fn write_commit_graph(
    repo: &Repo,
    perm: &mut RepoExclusive,
    ctx: &OpContext,
) -> Result<OpOutcome<()>> {
    let _ = perm;
    ctx.cancel.check()?;
    let cmd = GitCommand::new(repo, CommandKind::Mutation)
        .args([
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--progress",
        ])
        .with_progress();
    Ok(run_one(repo, cmd, ctx, OpClass::Generic)?.finish((), RefreshSet::LOG))
}
