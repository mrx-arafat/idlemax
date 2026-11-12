//! Domain: AI workers, one coding agent process idlemax starts for one task: the CLI, the
//! action of one worker job and the `worker` section that turns a job into a launch. Pure.

pub mod brief;
pub mod checks;
pub mod control;
pub mod launch;
pub mod name;
pub mod pace;
pub mod report;
pub mod run;

use serde::{Deserialize, Serialize};

pub type RunId = i64;

/// The agent CLI a worker or a reviewer runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCli {
    #[default]
    Claude,
    Codex,
    Opencode,
    Agy,
}

impl WorkerCli {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerCli::Claude => "claude",
            WorkerCli::Codex => "codex",
            WorkerCli::Opencode => "opencode",
            WorkerCli::Agy => "agy",
        }
    }
}

/// What one worker job does for its Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAction {
    /// A first start, or a start over the existing worktree when no session is stored.
    #[default]
    Launch,
    /// Continue the stored session (D11).
    Resume,
    Keep,
    KeepMerge,
    Discard,
}

impl WorkerAction {
    /// True for the actions that run an agent (`launch` and `resume`).
    pub fn is_launch(self) -> bool {
        matches!(self, WorkerAction::Launch | WorkerAction::Resume)
    }
}

/// The agent that reviews a high-risk diff, always another provider than the author (D8).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reviewer {
    pub cli: WorkerCli,
    pub model: String,
    pub account: String,
}

/// The project's check on a base commit, from Spec 1's check job (A11).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Baseline {
    pub base_commit: String,
    /// None when the check did not give a result.
    pub passed: Option<bool>,
}

/// The `worker` section of a `JobSpec`: one launch, resume, keep or discard of a Run. Only
/// the coordinator's dispatcher sets it. The brief travels in `JobSpec.stdin`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerSpec {
    pub run: RunId,
    pub action: WorkerAction,
    pub project: String,
    /// The task slug.
    pub task: String,
    pub task_title: String,
    /// Sent to the worker and the reviewer as text; idlemax never runs it.
    pub done_check: String,
    /// The project's checkout.
    pub repo: String,
    /// None means the checkout's current branch.
    pub base_branch: Option<String>,
    /// Set for every action except a first `launch`.
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub cli: WorkerCli,
    pub model: String,
    pub session: Option<String>,
    /// Argv, never a shell line (A4).
    pub setup: Vec<String>,
    pub check: Vec<String>,
    pub baseline: Option<Baseline>,
    pub scope_paths: Vec<String>,
    pub off_limits: Vec<String>,
    pub core_paths: Vec<String>,
    pub reviewer: Option<Reviewer>,
    pub review_diff_lines: u64,
    pub blocked_secs: u64,
    pub setup_secs: u64,
    pub check_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::report::{CheckFacts, WorkerPhase, WorkerReport};
    use super::*;
    use crate::control::{admit_job, ControlError};
    use crate::protocol::JobUpdate;
    use crate::quota::QuotaConfig;
    use crate::types::{CostClass, JobSpec, SpecError};

    fn worker_job() -> JobSpec {
        JobSpec {
            name: "im-demo-fix-1".into(),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some("me".into()),
            worker: Some(WorkerSpec {
                run: 7,
                project: "demo".into(),
                cli: WorkerCli::Codex,
                scope_paths: vec!["src/lib.rs".into()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_worker_section_needs_cost_ai() {
        assert_eq!(worker_job().validate(), Ok(()));
        let mut spec = worker_job();
        spec.cost = CostClass::CpuLight;
        spec.ai_account = None;
        assert_eq!(spec.validate(), Err(SpecError::WorkerWithoutAi));
    }

    #[test]
    fn admit_job_refuses_any_worker_section() {
        let err = admit_job(&worker_job(), &QuotaConfig::default()).unwrap_err();
        assert_eq!(err, ControlError::DispatcherOnly("worker"));
        assert_eq!(err.to_string(), "worker is set by the dispatcher only");
    }

    #[test]
    fn worker_paths_are_bounded() {
        let mut spec = worker_job();
        if let Some(w) = spec.worker.as_mut() {
            w.scope_paths = vec!["a".into(); 65];
        }
        assert!(matches!(spec.validate(), Err(SpecError::OverLimit { .. })));
    }

    #[test]
    fn the_worker_key_is_absent_unless_set_and_survives_a_round_trip() {
        let plain = serde_json::to_value(JobSpec::default()).unwrap();
        assert!(plain.get("worker").is_none());
        let update = serde_json::to_value(JobUpdate::default()).unwrap();
        assert!(update.get("worker").is_none());

        let spec = worker_job();
        let back: JobSpec = serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
        let update = JobUpdate {
            worker: Some(Box::new(WorkerReport {
                run: 7,
                phase: WorkerPhase::Done,
                session: Some("s".into()),
                facts: Some(CheckFacts {
                    commits: 1,
                    ..Default::default()
                }),
                end_text: Some("limit".into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        let back: JobUpdate =
            serde_json::from_value(serde_json::to_value(&update).unwrap()).unwrap();
        assert_eq!(back, update);
    }
}
