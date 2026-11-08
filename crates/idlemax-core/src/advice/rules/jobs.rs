//! Rules about jobs: parked or finally failed jobs nobody has replaced. Part of the advisor
//! rules (domain layer, pure).

use crate::advice::inputs::AdviceInput;
use crate::advice::text::{self, DAY_MS};
use crate::types::{Job, JobStatus, Recommendation, RecommendationKind};

use super::advice;

const PROBLEM_MAX_AGE_MS: i64 = 7 * DAY_MS;

/// Parked or finally failed jobs that no later run of the same job replaced.
pub(super) fn problem_jobs(input: &AdviceInput) -> Vec<Recommendation> {
    let recent =
        |j: &&Job| input.now_ms - j.finished_ms.unwrap_or(j.created_ms) <= PROBLEM_MAX_AGE_MS;
    input
        .jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Parked | JobStatus::Failed))
        .filter(recent)
        .filter(|j| !superseded(input, j))
        .map(|j| {
            let mut evidence = vec![format!("{} after {} attempt(s)", j.status, j.attempts)];
            evidence.extend(j.last_error.as_deref().map(|e| format!("last error: {}", text::clip(e, 200))));
            evidence.extend(j.exit_code.map(|c| format!("exit code {c}")));
            evidence.extend(j.device.as_deref().map(|d| format!("ran on {d}")));
            evidence.extend(j.output_path.as_deref().map(|p| format!("output: {p}")));
            Recommendation {
                title: text::clip(&format!("Fix job #{} {} ({})", j.id, j.spec.name, j.status), 120),
                why: "It will not run again on its own. Fix the cause, then accept to queue it again."
                    .into(),
                evidence,
                suggested_job: Some(j.spec.clone()),
                ..advice(input, format!("job_problem:{}", j.id), RecommendationKind::Fix, 0.9)
            }
        })
        .collect()
}

fn superseded(input: &AdviceInput, job: &Job) -> bool {
    input.jobs.iter().any(|o| {
        o.spec.name == job.spec.name
            && o.created_ms > job.created_ms
            && (o.status == JobStatus::Succeeded || !o.status.is_terminal())
    })
}
