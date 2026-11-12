//! Leasing on a heartbeat: what the scheduler picks from, the plain jobs the guard admits
//! and the dispatcher's own job the worker lines admit. Adapter over the domain scheduler.

use idlemax_core::control::{device_gate, is_held, may_assign, paused_accounts};
use idlemax_core::dispatch::room::lines_verdict;
use idlemax_core::protocol::Heartbeat;
use idlemax_core::scheduler::{lease_deadline, pick, pick_lines, SchedContext};
use idlemax_core::types::{ControlScope, EventKind, Job, JobId, JobStatus, UnixMs};
use idlemax_core::Config;

use super::super::http::ApiFailure;
use super::super::jobs::quota_views;
use crate::coord::store::{Store, StoreError};

/// What the scheduler picks from: every active job plus every queued job no pause holds.
/// `why` explains from the same list, so the two cannot disagree about the queue.
fn scheduler_jobs(store: &Store, paused: &[ControlScope]) -> Result<Vec<Job>, StoreError> {
    Ok(scheduler_jobs_and_held(store, paused)?.0)
}

/// `scheduler_jobs`, and the queued jobs a pause holds back that the scheduler never sees, from
/// one read of the queue (each queued job's spec is parsed once).
pub(in crate::coord::api) fn scheduler_jobs_and_held(
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
pub(super) fn assign_jobs(
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
/// it leases while the owner works (A16). It does not depend on the guard verdict at all,
/// and a device switched off leases none (D19).
fn lease_lines(
    store: &mut Store,
    cfg: &Config,
    ctx: &SchedContext<'_>,
    hb: &Heartbeat,
    jobs: &[Job],
    now: UnixMs,
) -> Result<Option<Job>, ApiFailure> {
    let gated = store
        .device(&hb.device.id, now, i64::MAX)?
        .is_none_or(|d| device_gate(&d).is_some());
    if gated {
        return Ok(None);
    }
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
