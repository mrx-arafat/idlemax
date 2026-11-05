//! Agent-facing routes: heartbeat (lease renewal, control commands, job assignment) and
//! job updates. Adapter: reads the store, asks the domain, persists the result.

use axum::extract::State;
use axum::Json;
use idlemax_core::advice::is_ai_pass;
use idlemax_core::control::{
    came_online, is_held, may_assign, paused_accounts, reconcile_heartbeat, transition_event,
};
use idlemax_core::protocol::{Heartbeat, HeartbeatReply, JobUpdate, JobUpdateReply};
use idlemax_core::scheduler::{apply_update, lease_deadline, pick, SchedContext};
use idlemax_core::types::{ControlScope, EventKind, Job, JobStatus, UnixMs};
use idlemax_core::Config;

use super::http::{ApiFailure, JsonBody};
use super::jobs::quota_views;
use super::offline_after_ms;
use super::{advice, AppState};
use crate::coord::store::{NewEvent, Store, StoreError, UpdateWrites};
use crate::state::now_ms;

/// `POST /v1/agent/heartbeat`.
pub async fn heartbeat(
    State(app): State<AppState>,
    JsonBody(hb): JsonBody<Heartbeat>,
) -> Result<Json<HeartbeatReply>, ApiFailure> {
    if hb.device.id.trim().is_empty() {
        return Err(ApiFailure::BadRequest("device.id is empty".into()));
    }
    let now = now_ms();
    let reply = app
        .with_store(move |store, cfg| on_heartbeat(store, cfg, &hb, now))
        .await?;
    Ok(Json(reply))
}

/// `POST /v1/agent/job-update`. A succeeded advisor AI pass feeds its output to the advisor.
/// The reply acks the update's `seq` once it is stored, or was stored before.
pub async fn job_update(
    State(app): State<AppState>,
    JsonBody(update): JsonBody<JobUpdate>,
) -> Result<Json<JobUpdateReply>, ApiFailure> {
    let now = now_ms();
    let seq = update.seq;
    let ai_output = app
        .with_store(move |store, _| on_job_update(store, &update, now))
        .await?;
    if let Some(output) = ai_output {
        advice::ingest_ai_output(&app, output, now).await;
    }
    Ok(Json(JobUpdateReply {
        acked_seq: (seq != 0).then_some(seq),
    }))
}

pub(super) fn on_heartbeat(
    store: &mut Store,
    cfg: &Config,
    hb: &Heartbeat,
    now: UnixMs,
) -> Result<HeartbeatReply, ApiFailure> {
    let device = &hb.device.id;
    let previous_seen = store.record_heartbeat(hb, now)?;
    if came_online(previous_seen, now, offline_after_ms(cfg)) {
        let msg = format!(
            "{device} online (host {}, agent {})",
            hb.device.hostname, hb.device.agent_version
        );
        store.append_event(EventKind::DeviceOnline, Some(device), None, &msg, now)?;
    }
    let paused = store.paused_scopes()?;
    let mut stored = Vec::with_capacity(hb.running.len());
    for r in &hb.running {
        stored.extend(store.get_job(r.id)?);
    }
    let plan = reconcile_heartbeat(device, &hb.running, &stored, &paused, now);
    store.update_jobs(&plan.renewed)?;
    for job in &plan.renewed {
        if let Some(before) = stored.iter().find(|j| j.id == job.id) {
            log_transition(store, before, job, now)?;
        }
    }
    let assign = assign_jobs(store, cfg, hb, &paused, now)?;
    Ok(HeartbeatReply {
        assign,
        commands: plan.commands,
    })
}

/// Leases what the scheduler picks; the store's compare-and-set settles any race.
fn assign_jobs(
    store: &mut Store,
    cfg: &Config,
    hb: &Heartbeat,
    paused: &[ControlScope],
    now: UnixMs,
) -> Result<Vec<Job>, ApiFailure> {
    let device = &hb.device.id;
    let mut jobs = store.active_jobs()?;
    jobs.extend(
        store
            .queued_jobs()?
            .into_iter()
            .filter(|j| !is_held(paused, j)),
    );
    let quota = quota_views(store, cfg, now)?;
    let accounts = paused_accounts(paused);
    let ctx = SchedContext {
        now_ms: now,
        device: &hb.device,
        guard: &hb.guard,
        running: &hb.running,
        quota: &quota,
        paused: !may_assign(paused, device, hb.local_paused),
        paused_accounts: &accounts,
    };
    let mut leased = Vec::new();
    for id in pick(&ctx, &jobs) {
        let Some(job) = jobs.iter().find(|j| j.id == id) else {
            continue;
        };
        match store.lease_job(id, device, lease_deadline(job, now), now) {
            Ok(Some(job)) => {
                let msg = format!(
                    "{} leased to {device} (attempt {})",
                    job.spec.name, job.attempts
                );
                store.append_event(EventKind::JobLeased, Some(device), Some(id), &msg, now)?;
                leased.push(job);
            }
            // No longer queued, or its account got leased meanwhile: skip, next heartbeat retries.
            Ok(None) | Err(StoreError::AccountLeased(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(leased)
}

/// Applies an agent's job update once per (device, seq): the job state, the transition
/// event and the quota side effects commit together, and a replay changes nothing.
/// Returns the output of a succeeded advisor AI pass.
pub(super) fn on_job_update(
    store: &mut Store,
    update: &JobUpdate,
    now: UnixMs,
) -> Result<Option<String>, ApiFailure> {
    let job = store
        .get_job(update.id)?
        .ok_or_else(|| ApiFailure::NotFound(format!("job {} not found", update.id)))?;
    let limit_hit = job
        .spec
        .ai_account
        .as_deref()
        .filter(|_| update.rate_limited);
    let mut events = Vec::new();
    if let Some(account) = limit_hit {
        events.push(NewEvent {
            kind: EventKind::Quota,
            device: Some(update.device.clone()),
            job: Some(job.id),
            message: format!("{account}: usage limit hit, no more AI jobs this window"),
        });
    }
    let next = apply_update(&job, update, now);
    if let Some(next) = &next {
        events.extend(transition_entry(&job, next));
    }
    let writes = UpdateWrites {
        limit_hit,
        job: next.as_ref(),
        events: &events,
    };
    if !store.commit_update(&update.device, update.seq, &writes, now)? {
        tracing::debug!(job = job.id, device = %update.device, seq = update.seq, "replayed job update ignored");
        return Ok(None);
    }
    let Some(next) = next else {
        tracing::debug!(job = job.id, device = %update.device, "stale job update ignored");
        return Ok(None);
    };
    let ai_done = next.status == JobStatus::Succeeded && is_ai_pass(&job.spec);
    Ok(update.output_tail.clone().filter(|_| ai_done))
}

/// One event per stored status change.
pub(super) fn log_transition(
    store: &mut Store,
    before: &Job,
    after: &Job,
    now: UnixMs,
) -> Result<(), StoreError> {
    if let Some(e) = transition_entry(before, after) {
        store.append_event(e.kind, e.device.as_ref(), e.job, &e.message, now)?;
    }
    Ok(())
}

/// The event for a stored status change, if that change is one worth logging.
fn transition_entry(before: &Job, after: &Job) -> Option<NewEvent> {
    let kind = transition_event(before.status, after.status)?;
    let mut message = format!("{}: {}", after.spec.name, after.status);
    if let Some(code) = after.exit_code.filter(|_| after.status.is_terminal()) {
        message.push_str(&format!(" (exit {code})"));
    }
    if let Some(err) = after
        .last_error
        .as_deref()
        .filter(|_| after.status != JobStatus::Running)
    {
        message.push_str(&format!(": {err}"));
    }
    Some(NewEvent {
        kind,
        device: after.device.clone().or_else(|| before.device.clone()),
        job: Some(after.id),
        message,
    })
}
