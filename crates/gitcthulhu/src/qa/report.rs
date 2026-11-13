//! The run report: `report.txt` (one line per step) and `report.json`, written into the output directory.

use std::path::{Path, PathBuf};

/// The outcome of one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The step did what it says.
    Pass,
    /// The step failed; the run goes on, the exit code becomes 1.
    Fail,
    /// The step could not run in this environment (no failure).
    Skip,
}

impl Status {
    /// `PASS`, `FAIL` or `SKIP`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

/// One reported step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    /// 1-based line in the scenario (0 for run-level problems).
    pub line: usize,
    /// The step's text.
    pub step: String,
    /// The outcome.
    pub status: Status,
    /// What the step reported (or why it failed).
    pub detail: String,
}

/// The whole run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The scenario's file name.
    pub scenario: String,
    /// The steps in order.
    pub steps: Vec<StepResult>,
    /// The screenshots written, in order.
    pub shots: Vec<PathBuf>,
    /// Extra remarks (error toasts seen, the watchdog).
    pub notes: Vec<String>,
}

impl Report {
    /// Adds a step.
    pub fn push(&mut self, line: usize, step: &str, status: Status, detail: impl Into<String>) {
        self.steps.push(StepResult {
            line,
            step: step.to_owned(),
            status,
            detail: detail.into(),
        });
    }

    /// Number of steps with `status`.
    pub fn count(&self, status: Status) -> usize {
        self.steps.iter().filter(|s| s.status == status).count()
    }

    /// 0 without a failed step, else 1.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.count(Status::Fail) > 0)
    }

    /// `report.txt`.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for step in &self.steps {
            text.push_str(&format!(
                "{} {} {}",
                step.status.as_str(),
                step.line,
                step.step
            ));
            if !step.detail.is_empty() {
                text.push_str(&format!(" -- {}", step.detail));
            }
            text.push('\n');
        }
        for shot in &self.shots {
            text.push_str(&format!("SHOT {}\n", shot.display()));
        }
        for note in &self.notes {
            text.push_str(&format!("NOTE {note}\n"));
        }
        text.push_str(&format!(
            "# {}: {} pass, {} fail, {} skip\n",
            self.scenario,
            self.count(Status::Pass),
            self.count(Status::Fail),
            self.count(Status::Skip)
        ));
        text
    }

    /// `report.json`.
    pub fn to_json(&self) -> String {
        let steps: Vec<serde_json::Value> = self
            .steps
            .iter()
            .map(|s| {
                serde_json::json!({
                    "line": s.line,
                    "step": s.step,
                    "status": s.status.as_str(),
                    "detail": s.detail,
                })
            })
            .collect();
        let value = serde_json::json!({
            "scenario": self.scenario,
            "pass": self.count(Status::Pass),
            "fail": self.count(Status::Fail),
            "skip": self.count(Status::Skip),
            "exit_code": self.exit_code(),
            "shots": self.shots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "notes": self.notes,
            "steps": steps,
        });
        serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
    }

    /// Writes both files into `dir`.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("report.txt"), self.to_text())?;
        std::fs::write(dir.join("report.json"), self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn exit_code_follows_failures_only() {
        let mut report = Report {
            scenario: "s.qa".into(),
            ..Default::default()
        };
        report.push(1, "wait idle", Status::Pass, "");
        report.push(2, "shot a", Status::Skip, "no window handle");
        assert_eq!(report.exit_code(), 0, "skips do not fail the run");
        report.push(3, "assert no error toast", Status::Fail, "boom");
        assert_eq!(report.exit_code(), 1, "a failed step fails the run");
    }

    #[test]
    fn text_and_json_list_steps_shots_and_notes() {
        let mut report = Report {
            scenario: "s.qa".into(),
            ..Default::default()
        };
        report.push(4, "shot a", Status::Pass, "1280x720");
        report.shots.push(PathBuf::from("out/a.png"));
        report.notes.push("watch out".into());
        let text = report.to_text();
        assert!(
            text.contains("PASS 4 shot a -- 1280x720"),
            "step line: {text}"
        );
        assert!(text.contains("SHOT out/a.png"), "shot line: {text}");
        assert!(text.contains("NOTE watch out"), "note line: {text}");
        assert!(
            text.contains("# s.qa: 1 pass, 0 fail, 0 skip"),
            "summary: {text}"
        );
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).expect("valid json");
        assert_eq!(json["pass"], 1, "pass count");
        assert_eq!(json["shots"][0], "out/a.png", "shots listed");
    }

    #[test]
    fn writes_both_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        Report::default().write(dir.path()).expect("written");
        assert!(dir.path().join("report.txt").is_file(), "report.txt");
        assert!(dir.path().join("report.json").is_file(), "report.json");
    }
}
