//! Turning finished `git` commands into an [`OpOutcome`]: the one place that merges command records, hook runs and
//! post-hook warnings of everything an operation ran.
//!
//! The executor already attributes failing post hooks (`GitOutput::warnings`), so no module needs to look at the hook
//! runs itself.

use crate::{
    CommandLog, OpClass, OpContext, OpOutcome, OpWarning, RefreshSet, Repo, Result,
    cli::{self, GitCommand, GitOutput},
};

/// Collects the logs and warnings of the `git` commands one operation ran.
#[derive(Default)]
pub(crate) struct Collected {
    log: CommandLog,
    warnings: Vec<OpWarning>,
}

impl Collected {
    /// Record one finished command.
    pub(crate) fn push(&mut self, out: GitOutput) {
        self.log.commands.push(out.record);
        self.log.hooks.extend(out.hooks);
        self.warnings.extend(out.warnings);
    }

    /// Record a finished command that the caller keeps.
    pub(crate) fn record(&mut self, out: &GitOutput) {
        self.log.commands.push(out.record.clone());
        self.log.hooks.extend(out.hooks.iter().cloned());
        self.warnings.extend(out.warnings.iter().cloned());
    }

    /// Record a finished command and hand its output back for parsing.
    pub(crate) fn add(&mut self, out: GitOutput) -> GitOutput {
        self.record(&out);
        out
    }

    /// Add a warning of the operation itself.
    pub(crate) fn warn(&mut self, warning: OpWarning) {
        self.warnings.push(warning);
    }

    /// The outcome of the operation: `value` plus everything collected.
    pub(crate) fn finish<T>(self, value: T, refresh: RefreshSet) -> OpOutcome<T> {
        OpOutcome {
            value,
            refresh,
            log: self.log,
            warnings: self.warnings,
        }
    }
}

/// Wrap the result of one finished git command into an [`OpOutcome`].
pub(crate) fn single<T>(value: T, refresh: RefreshSet, out: GitOutput) -> OpOutcome<T> {
    let mut collected = Collected::default();
    collected.push(out);
    collected.finish(value, refresh)
}

/// An outcome for an operation that needed no command at all.
pub(crate) fn no_op<T>(value: T) -> OpOutcome<T> {
    Collected::default().finish(value, RefreshSet::empty())
}

/// `cli::run_checked`, collected into a fresh [`Collected`].
pub(crate) fn run_one(
    repo: &Repo,
    cmd: GitCommand,
    ctx: &OpContext,
    class: OpClass,
) -> Result<Collected> {
    let mut collected = Collected::default();
    collected.push(cli::run_checked(repo, cmd, ctx, class)?);
    Ok(collected)
}
