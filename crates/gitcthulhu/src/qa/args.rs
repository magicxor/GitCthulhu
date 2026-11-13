//! The `--qa-*` command-line flags of a qa build.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// Default wall-clock limit of a scenario run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// The parsed QA flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QaArgs {
    /// `--qa-scenario <file>`: the scenario to run (required: a qa build never starts an interactive window).
    pub scenario: Option<PathBuf>,
    /// `--qa-out <dir>`: where the report and the screenshots go (required with a scenario).
    pub out_dir: Option<PathBuf>,
    /// `--qa-timeout <seconds>`: the watchdog limit of the whole run.
    pub timeout: Duration,
}

impl Default for QaArgs {
    fn default() -> Self {
        Self {
            scenario: None,
            out_dir: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Parses the process arguments (without the program name); other arguments are ignored. `--flag value` and
/// `--flag=value` are accepted.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<QaArgs, String> {
    let mut parsed = QaArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        let Some(flag) = text.strip_prefix("--qa-") else {
            continue;
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(OsString::from(value))),
            None => (flag, None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("--qa-{name} needs a value"))
        };
        match name {
            "scenario" => parsed.scenario = Some(PathBuf::from(value()?)),
            "out" => parsed.out_dir = Some(PathBuf::from(value()?)),
            "timeout" => {
                let value = value()?;
                let seconds: u64 = value
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| "--qa-timeout takes whole seconds".to_string())?;
                parsed.timeout = Duration::from_secs(seconds.max(1));
            }
            other => return Err(format!("unknown flag --qa-{other}")),
        }
    }
    // No unattended interactive window: without a scenario there is no watchdog and nothing ends the run, and the
    // file dialogs could open repositories outside the sandbox.
    if parsed.scenario.is_none() {
        return Err(
            "a qa build only runs scenarios: --qa-scenario <file> --qa-out <dir> (use tools/qa/run-gui.ps1)".into(),
        );
    }
    if parsed.out_dir.is_none() {
        return Err("--qa-scenario needs --qa-out <dir>".into());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn parse_strs(args: &[&str]) -> Result<QaArgs, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_scenario_out_and_timeout() {
        let args = parse_strs(&[
            "--qa-scenario",
            "a.qa",
            "--qa-out=out",
            "--qa-timeout",
            "30",
        ])
        .expect("valid flags");
        assert_eq!(args.scenario, Some(PathBuf::from("a.qa")), "scenario");
        assert_eq!(args.out_dir, Some(PathBuf::from("out")), "inline value");
        assert_eq!(args.timeout, Duration::from_secs(30), "timeout");
    }

    #[test]
    fn a_qa_build_needs_a_scenario() {
        let err = parse_strs(&["C:/some/repo"]).expect_err("no interactive qa run");
        assert!(err.contains("--qa-scenario"), "names the flag: {err}");
        let args = parse_strs(&["C:/some/repo", "--qa-scenario", "a.qa", "--qa-out", "o"])
            .expect("other arguments are ignored");
        assert_eq!(args.scenario, Some(PathBuf::from("a.qa")), "scenario");
    }

    #[test]
    fn a_scenario_requires_an_output_directory() {
        assert!(
            parse_strs(&["--qa-scenario", "a.qa"]).is_err(),
            "no --qa-out"
        );
    }

    #[test]
    fn unknown_flags_and_missing_values_are_errors() {
        assert!(parse_strs(&["--qa-frobnicate"]).is_err(), "unknown flag");
        assert!(parse_strs(&["--qa-out"]).is_err(), "missing value");
        assert!(parse_strs(&["--qa-timeout", "soon"]).is_err(), "bad number");
    }
}
