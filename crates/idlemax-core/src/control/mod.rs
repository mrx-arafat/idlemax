//! Domain: coordinator control semantics. Applies pause, resume, stop-all and cancel to the
//! paused-scope set and to jobs, derives the agent commands each heartbeat reply carries,
//! and admits new jobs. Pure: no I/O, no clock.
//!
//! Commands are level-triggered: every heartbeat re-derives them from stored state, so a lost
//! reply heals on the next heartbeat. Stop-all and cancel take effect in the store at once.

use crate::protocol::{
    name_problem, AgentCommand, ControlRequest, RunningJob, MAX_DEVICE_ID_CHARS,
};
use crate::quota::QuotaConfig;
use crate::scheduler::lease_deadline;
use crate::types::{
    ControlAction, ControlScope, EventKind, Job, JobId, JobSpec, JobStatus, SpecError, UnixMs,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("cancel needs a job scope")]
    CancelNeedsJob,
    #[error("job {0} not found")]
    UnknownJob(JobId),
    #[error("job {0} already finished")]
    JobFinished(JobId),
    #[error("the local API accepts only global pause, resume and stop_all")]
    NotLocal,
    #[error("invalid job: {0}")]
    InvalidJob(#[from] SpecError),
    #[error("ai_account {0:?} is not in [[quota.accounts]]")]
    UnknownAccount(String),
    #[error("{0}")]
    InvalidDevice(String),
}

/// What one control request changes. The adapter persists it and logs `summary`.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlPlan {
    /// Scope to add to (`true`) or remove from (`false`) the paused set.
    pub pause: Option<(ControlScope, bool)>,
    /// Changed jobs, already updated.
    pub jobs: Vec<Job>,
    /// One line for the event log.
    pub summary: String,
}

/// What one heartbeat reply carries back, plus the stored jobs to persist.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeartbeatPlan {
    pub commands: Vec<AgentCommand>,
    /// Reported jobs this device still holds: status synced from the report, lease renewed.
    pub renewed: Vec<Job>,
}

/// True when `scope` covers `job`. A device scope covers only jobs leased to that device
/// (a queued job may still name the device it last ran on).
pub fn in_scope(scope: &ControlScope, job: &Job) -> bool {
    match scope {
        ControlScope::Global => true,
        ControlScope::Device(d) => job.status.is_active() && job.device.as_ref() == Some(d),
        ControlScope::Account(a) => job.spec.ai_account.as_ref() == Some(a),
        ControlScope::Job(id) => job.id == *id,
    }
}

/// True when any paused scope holds `job`.
pub fn is_held(paused: &[ControlScope], job: &Job) -> bool {
    paused.iter().any(|s| in_scope(s, job))
}

pub fn global_paused(paused: &[ControlScope]) -> bool {
    paused.contains(&ControlScope::Global)
}

pub fn paused_accounts(paused: &[ControlScope]) -> Vec<String> {
    paused
        .iter()
        .filter_map(|s| match s {
            ControlScope::Account(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

/// Admission for `POST /v1/jobs`: a valid spec whose AI account is configured, since an
/// unknown account is never spendable and the job would wait forever.
pub fn admit_job(spec: &JobSpec, quota: &QuotaConfig) -> Result<(), ControlError> {
    spec.validate()?;
    match &spec.ai_account {
        Some(a) if quota.account(a).is_none() => Err(ControlError::UnknownAccount(a.clone())),
        _ => Ok(()),
    }
}

/// Nothing new is leased to a device paused globally, by device scope, or on the device itself.
pub fn may_assign(paused: &[ControlScope], device: &str, local_paused: bool) -> bool {
    let device_paused = paused.iter().any(|s| match s {
        ControlScope::Global => true,
        ControlScope::Device(d) => d == device,
        _ => false,
    });
    !local_paused && !device_paused
}

/// Applies a control request. `jobs` must hold the active jobs, plus the target job for a
/// job scope whatever its status; jobs outside the scope are ignored.
pub fn plan_control(
    req: &ControlRequest,
    jobs: &[Job],
    now_ms: UnixMs,
) -> Result<ControlPlan, ControlError> {
    // A pause flag is stored per name, so its name is checked; taking one away never is.
    if let (ControlScope::Device(id), false) = (&req.scope, req.action == ControlAction::Resume) {
        if let Some(why) = name_problem("device id", id, MAX_DEVICE_ID_CHARS) {
            return Err(ControlError::InvalidDevice(why));
        }
    }
    let note = format!("{}: {}", action_label(req.action), reason(req));
    match (req.action, &req.scope) {
        (ControlAction::Cancel, ControlScope::Job(id)) => {
            let job = open_target(*id, jobs)?;
            Ok(finish_one(
                req,
                finish(job, JobStatus::Cancelled, &note, now_ms),
            ))
        }
        (ControlAction::Cancel, _) => Err(ControlError::CancelNeedsJob),
        (ControlAction::StopAll, ControlScope::Job(id)) => {
            let job = open_target(*id, jobs)?;
            // A job that never started is cancelled, not killed.
            let status = if job.status.is_active() {
                JobStatus::Killed
            } else {
                JobStatus::Cancelled
            };
            Ok(finish_one(req, finish(job, status, &note, now_ms)))
        }
        (ControlAction::StopAll, scope) => {
            let killed: Vec<Job> = jobs
                .iter()
                .filter(|j| j.status.is_active() && in_scope(scope, j))
                .map(|j| finish(j, JobStatus::Killed, &note, now_ms))
                .collect();
            let summary = format!(
                "{} {}: {} job(s) killed",
                headline(req),
                reason(req),
                killed.len()
            );
            Ok(ControlPlan {
                pause: Some((scope.clone(), true)),
                jobs: killed,
                summary,
            })
        }
        (ControlAction::Pause, scope) => {
            if let ControlScope::Job(id) = scope {
                open_target(*id, jobs)?;
            }
            Ok(pause_plan(req, true))
        }
        (ControlAction::Resume, _scope) => Ok(pause_plan(req, false)),
    }
}

/// Commands for the jobs a device reports, and the jobs whose lease it renews.
/// `stored` holds the stored job for each reported id that still exists.
pub fn reconcile_heartbeat(
    device: &str,
    reported: &[RunningJob],
    stored: &[Job],
    paused: &[ControlScope],
    lease_secs: u64,
    now_ms: UnixMs,
) -> HeartbeatPlan {
    let mut plan = HeartbeatPlan::default();
    for r in reported {
        let held_here = stored.iter().find(|j| j.id == r.id).filter(|j| {
            (j.status.is_active() && j.device.as_deref() == Some(device))
                || (j.status == JobStatus::Queued
                    && j.device.is_none()
                    && j.attempts > 0
                    && matches!(r.status, JobStatus::Running | JobStatus::Paused))
        });
        let Some(job) = held_here else {
            // Unknown, finished, cancelled, stopped or re-leased elsewhere.
            plan.commands.push(AgentCommand::KillJob { id: r.id });
            continue;
        };
        if is_held(paused, job) {
            plan.commands.push(AgentCommand::PauseJob { id: r.id });
        } else if r.status == JobStatus::Paused {
            plan.commands.push(AgentCommand::ResumeJob { id: r.id });
        }
        let mut renewed = sync_reported(job, r, lease_secs, now_ms);
        renewed.device = Some(device.to_string());
        renewed.not_before_ms = None;
        plan.renewed.push(renewed);
    }
    plan
}

/// The agent is the source of truth for a job it holds: Running or Paused, started time.
fn sync_reported(job: &Job, r: &RunningJob, lease_secs: u64, now_ms: UnixMs) -> Job {
    let mut job = job.clone();
    if matches!(r.status, JobStatus::Running | JobStatus::Paused) {
        job.status = r.status;
    }
    job.started_ms = job.started_ms.or(Some(r.started_ms));
    job.lease_expires_ms = Some(lease_deadline(lease_secs, now_ms));
    job
}

/// The local agent API takes only Global pause, resume and stop_all.
/// Returns the `local_paused` value the agent shows once it applied the request.
pub fn local_paused_after(req: &ControlRequest) -> Result<bool, ControlError> {
    match (&req.scope, req.action) {
        (ControlScope::Global, ControlAction::Pause | ControlAction::StopAll) => Ok(true),
        (ControlScope::Global, ControlAction::Resume) => Ok(false),
        _ => Err(ControlError::NotLocal),
    }
}

/// Event kind for a stored status change; None when nothing changed.
pub fn transition_event(from: JobStatus, to: JobStatus) -> Option<EventKind> {
    if from == to {
        return None;
    }
    Some(match to {
        JobStatus::Queued => EventKind::JobQueued,
        JobStatus::Leased => EventKind::JobLeased,
        JobStatus::Running if from == JobStatus::Paused => EventKind::JobResumed,
        JobStatus::Running => EventKind::JobStarted,
        JobStatus::Paused => EventKind::JobPaused,
        JobStatus::Succeeded => EventKind::JobSucceeded,
        JobStatus::Failed => EventKind::JobFailed,
        JobStatus::Killed => EventKind::JobKilled,
        JobStatus::Cancelled => EventKind::JobCancelled,
        JobStatus::Parked => EventKind::JobParked,
    })
}

/// A heartbeat brings a device online when it is new or was silent past the offline limit.
pub fn came_online(
    previous_seen_ms: Option<UnixMs>,
    now_ms: UnixMs,
    offline_after_ms: i64,
) -> bool {
    previous_seen_ms.is_none_or(|seen| !device_online(seen, now_ms, offline_after_ms))
}

/// A device is online through the exact offline limit and offline once it passes.
pub fn device_online(last_seen_ms: UnixMs, now_ms: UnixMs, offline_after_ms: i64) -> bool {
    now_ms.saturating_sub(last_seen_ms) <= offline_after_ms
}

/// The transition is new only for a device already online that has gone silent.
pub fn offline_transition(
    was_online: bool,
    last_seen_ms: UnixMs,
    now_ms: UnixMs,
    offline_after_ms: i64,
) -> bool {
    was_online && !device_online(last_seen_ms, now_ms, offline_after_ms)
}

pub fn scope_label(scope: &ControlScope) -> String {
    match scope {
        ControlScope::Global => "global".into(),
        ControlScope::Device(d) => format!("device {d}"),
        ControlScope::Account(a) => format!("account {a}"),
        ControlScope::Job(id) => format!("job {id}"),
    }
}

fn action_label(action: ControlAction) -> &'static str {
    match action {
        ControlAction::Pause => "pause",
        ControlAction::Resume => "resume",
        ControlAction::StopAll => "stop_all",
        ControlAction::Cancel => "cancel",
    }
}

fn headline(req: &ControlRequest) -> String {
    format!("{} {}", action_label(req.action), scope_label(&req.scope))
}

fn reason(req: &ControlRequest) -> &str {
    req.reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("no reason given")
}

fn pause_plan(req: &ControlRequest, paused: bool) -> ControlPlan {
    let summary = format!("{} ({})", headline(req), reason(req));
    ControlPlan {
        pause: Some((req.scope.clone(), paused)),
        jobs: Vec::new(),
        summary,
    }
}

fn finish_one(req: &ControlRequest, job: Job) -> ControlPlan {
    let summary = format!("{} ({}): {}", headline(req), reason(req), job.status);
    ControlPlan {
        pause: None,
        jobs: vec![job],
        summary,
    }
}

fn target(id: JobId, jobs: &[Job]) -> Result<&Job, ControlError> {
    jobs.iter()
        .find(|j| j.id == id)
        .ok_or(ControlError::UnknownJob(id))
}

/// The target job, which must not be finished yet.
fn open_target(id: JobId, jobs: &[Job]) -> Result<&Job, ControlError> {
    let job = target(id, jobs)?;
    if job.status.is_terminal() {
        return Err(ControlError::JobFinished(id));
    }
    Ok(job)
}

fn finish(job: &Job, status: JobStatus, note: &str, now_ms: UnixMs) -> Job {
    Job {
        status,
        finished_ms: Some(now_ms),
        lease_expires_ms: None,
        not_before_ms: None,
        last_error: Some(note.to_string()),
        ..job.clone()
    }
}

mod scope;
pub use scope::unknown_scope;

#[cfg(test)]
mod recovery_tests;

#[cfg(test)]
mod tests {
    use super::{came_online, device_online, offline_transition};

    const SEEN: i64 = 1_800_000_000_000;
    const LIMIT: i64 = 90_000;

    #[test]
    fn offline_starts_after_the_limit() {
        assert!(device_online(SEEN, SEEN + LIMIT, LIMIT));
        assert!(!device_online(SEEN, SEEN + LIMIT + 1, LIMIT));
        assert!(came_online(Some(SEEN), SEEN + LIMIT + 1, LIMIT));
        assert!(!came_online(Some(SEEN), SEEN + LIMIT, LIMIT));
    }

    #[test]
    fn offline_transition_needs_a_previous_online_state() {
        assert!(offline_transition(true, SEEN, SEEN + LIMIT + 1, LIMIT));
        assert!(!offline_transition(false, SEEN, SEEN + LIMIT + 1, LIMIT));
        assert!(!offline_transition(true, SEEN, SEEN + LIMIT, LIMIT));
    }
}
