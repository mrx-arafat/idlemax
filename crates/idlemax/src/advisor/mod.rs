//! Adapter: advisor use cases for the coordinator. Merges the inputs the agents sent (`gh`
//! summary, session metadata) with the job templates, runs the `idlemax_core::advice`
//! domain, and turns a due AI pass into a `claude -p` job. Never touches the store: the
//! caller persists. The `github` and `sessions` readers run in the agent, on the host
//! that has `gh` and the session files (see `agent::advisor_feed`).

pub mod claude;
pub mod github;
pub mod sessions;
pub mod templates;

use idlemax_core::advice::{self, ai, AdviceError, AdviceInput, RefreshPlan, StoredInputs};
use idlemax_core::config::AdvisorConfig;
use idlemax_core::protocol::ClusterStatus;
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{Job, JobId, JobSpec, Recommendation, UnixMs};
use idlemax_core::Paths;

/// Coordinator state the advisor reads.
#[derive(Debug, Clone, Default)]
pub struct CoordState {
    pub status: ClusterStatus,
    /// Recent jobs in any status (newest 500 is plenty).
    pub jobs: Vec<Job>,
    pub quota: Vec<QuotaView>,
    /// Every device's newest advisor inputs, as the agents sent them.
    pub inputs: Vec<StoredInputs>,
}

/// What to persist: insert advice (id 0; Accepted ones were auto-accepted), mark `close`
/// ids dismissed, queue every spec in `queue` (auto-accepted jobs and the AI pass), and
/// mark `cancel` job ids Cancelled (stale queued AI passes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdviceChanges {
    pub insert: Vec<Recommendation>,
    pub close: Vec<i64>,
    pub queue: Vec<JobSpec>,
    pub cancel: Vec<JobId>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdvisorError {
    #[error(transparent)]
    Claude(#[from] claude::ClaudeError),
    #[error(transparent)]
    Advice(#[from] AdviceError),
    #[error("advisor background task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Re-runs the rules engine (and queues an AI pass when due).
/// `existing`: stored advice of the last 7 days in any status, for dedup.
pub async fn refresh(
    paths: &Paths,
    cfg: &AdvisorConfig,
    state: CoordState,
    existing: &[Recommendation],
    now_ms: UnixMs,
) -> Result<AdviceChanges, AdvisorError> {
    if !cfg.enabled {
        return Ok(AdviceChanges::default());
    }
    let jobs_dir = paths.jobs_dir();
    let templates = tokio::task::spawn_blocking(move || templates::load(&jobs_dir)).await?;
    let (github, sessions) = advice::merge_inputs(&state.inputs, now_ms);
    let input = AdviceInput {
        now_ms,
        status: state.status,
        jobs: state.jobs,
        quota: state.quota,
        github,
        sessions,
        templates,
    };
    Ok(changes(advice::plan_refresh(cfg, &input, existing)))
}

/// Turns a finished AI pass's stdout tail into advice.
pub async fn ingest_ai_output(
    paths: &Paths,
    cfg: &AdvisorConfig,
    status: &ClusterStatus,
    existing: &[Recommendation],
    output_tail: &str,
    now_ms: UnixMs,
) -> Result<AdviceChanges, AdvisorError> {
    let output = claude::structured_output(output_tail)?;
    let jobs_dir = paths.jobs_dir();
    let templates = tokio::task::spawn_blocking(move || templates::load(&jobs_dir)).await?;
    let device_ids: Vec<String> = status.devices.iter().map(|d| d.info.id.clone()).collect();
    let plan = advice::plan_ai_ingest(cfg, &templates, &device_ids, existing, &output, now_ms)?;
    Ok(changes(plan))
}

fn changes(plan: RefreshPlan) -> AdviceChanges {
    let mut queue = plan.queue;
    queue.extend(
        plan.ai_pass
            .map(|pass| claude::pass_job(&pass, &ai::schema())),
    );
    AdviceChanges {
        insert: plan.insert,
        close: plan.close,
        queue,
        cancel: plan.cancel,
    }
}
