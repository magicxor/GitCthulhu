//! Summary of CI checks (port of the old GUI's `summarize_checks` and `check_state`).

use super::{ChecksSummary, CiCheck, CiConclusion, CiStatus};

/// How one check stands.
enum State {
    Passed,
    Running,
    Failed,
    Queued,
}

fn state(check: &CiCheck) -> State {
    match &check.status {
        CiStatus::Complete { conclusion, .. } => match conclusion {
            CiConclusion::Success | CiConclusion::Neutral | CiConclusion::Skipped => State::Passed,
            // A conclusion nobody knows is not shown as success.
            CiConclusion::Cancelled
            | CiConclusion::Failure
            | CiConclusion::TimedOut
            | CiConclusion::ActionRequired
            | CiConclusion::Unknown => State::Failed,
        },
        CiStatus::InProgress => State::Running,
        CiStatus::Queued | CiStatus::Unknown => State::Queued,
    }
}

/// Count `checks` by state.
pub(super) fn summarize(checks: &[CiCheck]) -> ChecksSummary {
    checks
        .iter()
        .fold(ChecksSummary::default(), |mut sum, check| {
            match state(check) {
                State::Passed => sum.passed += 1,
                State::Running => sum.running += 1,
                State::Failed => sum.failed += 1,
                State::Queued => sum.queued += 1,
            }
            sum
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::CiOutput;

    fn check(status: CiStatus) -> CiCheck {
        CiCheck {
            id: 1,
            name: "build".into(),
            output: CiOutput::default(),
            started_at: None,
            status,
            head_sha: String::new(),
            url: String::new(),
            html_url: String::new(),
            details_url: String::new(),
            pull_requests: Vec::new(),
            reference: "main".into(),
            last_sync_at: chrono::NaiveDateTime::default(),
        }
    }

    fn complete(conclusion: CiConclusion) -> CiStatus {
        CiStatus::Complete {
            conclusion,
            completed_at: None,
        }
    }

    #[test]
    fn checks_are_counted_by_state() {
        let checks: Vec<_> = [
            complete(CiConclusion::Success),
            complete(CiConclusion::Neutral),
            complete(CiConclusion::Skipped),
            complete(CiConclusion::Failure),
            complete(CiConclusion::TimedOut),
            complete(CiConclusion::Cancelled),
            complete(CiConclusion::ActionRequired),
            complete(CiConclusion::Unknown),
            CiStatus::InProgress,
            CiStatus::Queued,
            CiStatus::Unknown,
        ]
        .into_iter()
        .map(check)
        .collect();
        assert_eq!(
            summarize(&checks),
            ChecksSummary {
                passed: 3,
                running: 1,
                failed: 5,
                queued: 2,
            },
            "success, neutral and skipped pass; every other conclusion fails; queued and unknown wait"
        );
        assert_eq!(
            summarize(&[]),
            ChecksSummary::default(),
            "no checks, nothing counted"
        );
    }
}
