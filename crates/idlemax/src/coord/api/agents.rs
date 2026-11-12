//! Agent-facing routes: heartbeat (lease renewal, control commands, job assignment) and
//! job updates. Adapter: reads the store, asks the domain, persists the result.

use axum::extract::State;
use axum::Json;
use idlemax_core::advice::{is_ai_pass, AdvisorInputs};
use idlemax_core::control::{
    came_online, is_held, may_assign, paused_accounts, reconcile_heartbeat, transition_event,
};
use idlemax_core::dispatch::orchestrator::is_pass;
use idlemax_core::dispatch::room::lines_verdict;
use idlemax_core::protocol::{
    name_problem, Heartbeat, HeartbeatReply, JobUpdate, JobUpdateReply, MAX_DEVICE_ID_CHARS,
};
use idlemax_core::scheduler::{apply_update, lease_deadline, pick, pick_lines, SchedContext};
use idlemax_core::types::{ControlScope, EventKind, Job, JobId, JobStatus, UnixMs};
use idlemax_core::worker::run::{after_job, Run, RunState};
use idlemax_core::Config;
use serde_json::{json, Value};

use super::budget::{worker_rest, WorkerRest};
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
    hb.check().map_err(ApiFailure::BadRequest)?;
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
    let (seq, job_id) = (update.seq, update.id);
    let live = std::sync::Arc::clone(app.live());
    let ai_output = app
        .with_store(move |store, cfg| {
            let rest = worker_rest(store, cfg, &live.current(), &update, now)?;
            let output = on_job_update_with(store, &update, rest, now)?;
            let pass = store.get_job(job_id)?.is_some_and(|j| is_pass(&j.spec));
            Ok::<_, ApiFailure>(output.map(|o| (o, pass)))
        })
        .await?;
    match ai_output {
        Some((output, true)) => super::plan::ingest_pass(&app, job_id, output, now).await,
        Some((output, false)) => advice::ingest_ai_output(&app, job_id, output, now).await,
        None => {}
    }
    Ok(Json(JobUpdateReply {
        acked_seq: (seq != 0).then_some(seq),
    }))
}

/// `POST /v1/agent/advisor-inputs`: keeps the newest copy per device for the advisor.
pub async fn advisor_inputs(
    State(app): State<AppState>,
    JsonBody(inputs): JsonBody<AdvisorInputs>,
) -> Result<Json<Value>, ApiFailure> {
    if inputs.device.trim().is_empty() {
        return Err(ApiFailure::BadRequest("device is empty".into()));
    }
    if let Some(why) = name_problem("device", &inputs.device, MAX_DEVICE_ID_CHARS) {
        return Err(ApiFailure::BadRequest(why));
    }
    let now = now_ms();
    app.with_store(move |store, _| {
        store.put_advisor_inputs(&inputs, now)?;
        Ok::<_, ApiFailure>(())
    })
    .await?;
    Ok(Json(json!({})))
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
    let plan = reconcile_heartbeat(
        device,
        &hb.running,
        &stored,
        &paused,
        cfg.coordinator.lease_secs,
        now,
    );
    store.update_jobs(&plan.renewed)?;
    for job in &plan.renewed {
        if let Some(before) = stored.iter().find(|j| j.id == job.id) {
            log_transition(store, before, job, now)?;
        }
    }
    let (assign, queued_work) = assign_jobs(store, cfg, hb, &paused, now)?;
    Ok(HeartbeatReply {
        assign,
        commands: plan.commands,
        queued_work,
    })
}

/// What the scheduler picks from: every active job plus every queued job no pause holds.
/// `why` explains from the same list, so the two cannot disagree about the queue.
pub(super) fn scheduler_jobs(
    store: &Store,
    paused: &[ControlScope],
) -> Result<Vec<Job>, StoreError> {
    Ok(scheduler_jobs_and_held(store, paused)?.0)
}

/// `scheduler_jobs`, and the queued jobs a pause holds back that the scheduler never sees, from
/// one read of the queue (each queued job's spec is parsed once).
pub(super) fn scheduler_jobs_and_held(
    store: &Store,
    paused: &[ControlScope],
) -> Result<(Vec<Job>, Vec<Job>), StoreError> {
    let mut jobs = store.active_jobs()?;
    let (held, visible): (Vec<Job>, Vec<Job>) = store
        .queued_jobs()?
        .into_iter()
        .partition(|j| is_held(paused, j));
    jobs.extend(visible);
    Ok((jobs, held))
}

/// Leases what the scheduler picks; the store's compare-and-set settles any race.
fn assign_jobs(
    store: &mut Store,
    cfg: &Config,
    hb: &Heartbeat,
    paused: &[ControlScope],
    now: UnixMs,
) -> Result<(Vec<Job>, bool), ApiFailure> {
    let device = &hb.device.id;
    let jobs = scheduler_jobs(store, paused)?;
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
    // Ask the same scheduler with presence satisfied, keeping all resource and control gates.
    let mut idle_guard = hb.guard.clone();
    if idle_guard.user_active {
        idle_guard.action = idle_guard.resource_action;
    }
    let queued_work = !pick(
        &SchedContext {
            guard: &idle_guard,
            ..ctx
        },
        &jobs,
    )
    .is_empty();
    let mut leased = Vec::new();
    for job in &jobs {
        // The owner's keep, merge and discard are never held by a pause (A10).
        let held = (ctx.paused || is_held(paused, job)) && !is_owner_action(job);
        if job.status == JobStatus::Leased
            && job.device.as_deref() == Some(device)
            && !held
            && !hb.running.iter().any(|r| r.id == job.id)
        {
            leased.push(job.clone());
        }
    }
    for id in pick(&ctx, &jobs) {
        leased.extend(lease_one(store, cfg, device, id, now)?);
    }
    leased.extend(lease_lines(store, cfg, &ctx, hb, &jobs, now)?);
    Ok((leased, queued_work))
}

/// The dispatcher's own job, admitted by the device's worker lines and not by the guard, so
/// it leases while the owner works (A16). It does not depend on the guard verdict at all.
fn lease_lines(
    store: &mut Store,
    cfg: &Config,
    ctx: &SchedContext<'_>,
    hb: &Heartbeat,
    jobs: &[Job],
    now: UnixMs,
) -> Result<Option<Job>, ApiFailure> {
    let lines = lines_verdict(hb.room.as_ref(), now);
    match pick_lines(ctx, &lines, jobs) {
        Some(id) => lease_one(store, cfg, &hb.device.id, id, now),
        None => Ok(None),
    }
}

/// A leased keep, merge or discard job of the owner's review.
fn is_owner_action(job: &Job) -> bool {
    job.spec
        .worker
        .as_ref()
        .is_some_and(|w| !w.action.is_launch())
}

/// Leases one picked job and logs it; None when it is no longer queued or its account got
/// leased meanwhile, and the next heartbeat retries.
fn lease_one(
    store: &mut Store,
    cfg: &Config,
    device: &str,
    id: JobId,
    now: UnixMs,
) -> Result<Option<Job>, ApiFailure> {
    let deadline = lease_deadline(cfg.coordinator.lease_secs, now);
    match store.lease_job(id, device, deadline, now) {
        Ok(Some(job)) => {
            let msg = format!(
                "{} leased to {device} (attempt {})",
                job.spec.name, job.attempts
            );
            let dev = device.to_string();
            store.append_event(EventKind::JobLeased, Some(&dev), Some(id), &msg, now)?;
            Ok(Some(job))
        }
        Ok(None) | Err(StoreError::AccountLeased(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Applies an agent's job update once per (device, seq): the job state, the transition
/// event and the quota side effects commit together, and a replay changes nothing.
/// Returns the output of a succeeded advisor AI pass to ingest: the first time, and again on
/// a replay while the pass has recorded no Advice event, which means the coordinator died
/// between storing the result and ingesting it (the ack is sent only after the ingest).
#[cfg(test)]
pub(super) fn on_job_update(
    store: &mut Store,
    update: &JobUpdate,
    now: UnixMs,
) -> Result<Option<String>, ApiFailure> {
    on_job_update_with(store, update, None, now)
}

/// `on_job_update` with the new rest a worker's limit message starts: its hit is stored by the
/// same write as an old AI job's, with one `Quota` event.
pub(super) fn on_job_update_with(
    store: &mut Store,
    update: &JobUpdate,
    rest: Option<WorkerRest>,
    now: UnixMs,
) -> Result<Option<String>, ApiFailure> {
    let job = store
        .get_job(update.id)?
        .ok_or_else(|| ApiFailure::NotFound(format!("job {} not found", update.id)))?;
    let old_hit = job
        .spec
        .ai_account
        .as_deref()
        .filter(|_| update.rate_limited && job.spec.worker.is_none());
    let mut events = Vec::new();
    if let Some(account) = old_hit {
        events.push(NewEvent {
            kind: EventKind::Quota,
            device: Some(update.device.clone()),
            job: Some(job.id),
            message: format!("{account}: usage limit hit, no more AI jobs this window"),
        });
    }
    events.extend(rest.as_ref().map(|r| r.event(&update.device, job.id)));
    let limit_hit = old_hit.or(rest.as_ref().map(|r| r.account.as_str()));
    let next = apply_update(&job, update, now);
    if let Some(next) = &next {
        events.extend(transition_entry(&job, next));
    }
    let run = match next {
        Some(_) => run_after(store, &job, update, now)?,
        None => None,
    };
    let writes = UpdateWrites {
        limit_hit,
        job: next.as_ref(),
        run: run.as_ref().map(|(after, before)| (after, *before)),
        events: &events,
    };
    if !store.commit_update(&update.device, update.seq, &writes, now)? {
        tracing::debug!(job = job.id, device = %update.device, seq = update.seq, "replayed job update ignored");
        return unfinished_ingest(store, &job, update);
    }
    // MISSING HOOK (Spec 2 Task 9): with a new rest, stop the account's live workers with
    // their state saved: `dispatch::stop_workers(store, &StopScope::Account(account), now)`.
    let Some(next) = next else {
        tracing::debug!(job = job.id, device = %update.device, "stale job update ignored");
        return Ok(None);
    };
    let ai_done = next.status == JobStatus::Succeeded && ingests(&job.spec);
    Ok(update.output_tail.clone().filter(|_| ai_done))
}

/// A worker job's Run as the update leaves it, with the state it had; None for a plain job
/// or an update that does not move the Run.
fn run_after(
    store: &Store,
    job: &Job,
    update: &JobUpdate,
    now: UnixMs,
) -> Result<Option<(Run, RunState)>, ApiFailure> {
    let Some(worker) = &job.spec.worker else {
        return Ok(None);
    };
    let Some(run) = store.get_run(worker.run)? else {
        return Ok(None);
    };
    let Some(mut next) = after_job(&run, job, update, now) else {
        return Ok(None);
    };
    // Each launch that ends records the account's readings, for the cost table (D10 item 1).
    if update.status.is_terminal() {
        let latest = store.latest_quota()?;
        next.quota_after = latest
            .into_iter()
            .filter(|s| s.account == run.account)
            .collect();
    }
    Ok(Some((next, run.state)))
}

/// The output of an advisor AI pass stored as succeeded whose advice was never recorded.
fn unfinished_ingest(
    store: &Store,
    job: &Job,
    update: &JobUpdate,
) -> Result<Option<String>, ApiFailure> {
    let ai_done = job.status == JobStatus::Succeeded
        && update.status == JobStatus::Succeeded
        && ingests(&job.spec);
    let output = update.output_tail.clone().filter(|_| ai_done);
    let ingested = if is_pass(&job.spec) {
        // A pass is ingested once its row has a result or an error.
        store
            .pass_for_job(job.id)?
            .is_none_or(|r| r.result.is_some() || r.error.is_some())
    } else {
        store.job_has_event(job.id, EventKind::Advice)?
    };
    if output.is_some() && ingested {
        return Ok(None);
    }
    Ok(output)
}

/// A job whose output the coordinator reads: the advisor's AI pass or the orchestrator pass.
fn ingests(spec: &idlemax_core::types::JobSpec) -> bool {
    is_ai_pass(spec) || is_pass(spec)
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
