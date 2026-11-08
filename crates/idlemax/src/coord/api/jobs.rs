//! Operator routes: cluster status, jobs, control, events and quota.
//! Adapter: reads the store, asks the domain, persists the result.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use idlemax_core::control::{
    admit_job, global_paused, paused_accounts, plan_control, unknown_scope,
};
use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::quota::{self, QuotaView, SpendContext, MINUTE_MS, WEEK_MINUTES};
use idlemax_core::types::{
    ControlAction, ControlScope, Event, EventKind, Job, JobId, JobSpec, JobStatus,
    RecommendationStatus, UnixMs,
};
use idlemax_core::{Config, VERSION};
use serde::Deserialize;

use super::agents::log_transition;
use super::http::{ApiFailure, JsonBody};
use super::{offline_after_ms, page_limit, AppState};
use crate::coord::store::{Added, Store, StoreError};
use crate::state::now_ms;

/// Quota inputs older than the longest window (a week) cannot matter.
const QUOTA_LOOKBACK_MS: i64 = WEEK_MINUTES as i64 * MINUTE_MS;

#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    status: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub(super) limit: Option<u32>,
}

/// `GET /v1/status`.
pub async fn status(State(app): State<AppState>) -> Result<Json<ClusterStatus>, ApiFailure> {
    let now = now_ms();
    Ok(Json(
        app.with_store(move |store, cfg| cluster_status(store, cfg, now))
            .await?,
    ))
}

/// `GET /v1/jobs?status=&limit=`, newest first.
pub async fn list(
    State(app): State<AppState>,
    query: Result<Query<JobsQuery>, QueryRejection>,
) -> Result<Json<Vec<Job>>, ApiFailure> {
    let Query(q) = query?;
    let status = match q.status.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => Some(s.parse::<JobStatus>().map_err(ApiFailure::BadRequest)?),
    };
    let limit = page_limit(q.limit);
    let jobs = app.with_store(move |store, _| Ok::<_, ApiFailure>(store.list_jobs(status, limit)?));
    Ok(Json(jobs.await?.into_iter().map(operator_job).collect()))
}

/// `GET /v1/jobs/{id}`.
pub async fn get_one(
    State(app): State<AppState>,
    path: Result<Path<JobId>, PathRejection>,
) -> Result<Json<Job>, ApiFailure> {
    let Path(id) = path?;
    let job = app
        .with_store(move |store, _| {
            store
                .get_job(id)?
                .ok_or_else(|| ApiFailure::NotFound(format!("job {id} not found")))
        })
        .await?;
    Ok(Json(operator_job(job)))
}

/// `POST /v1/jobs`: 201 with the queued job, or 200 with the unfinished job that already
/// holds the spec's `key`.
pub async fn add(
    State(app): State<AppState>,
    JsonBody(spec): JsonBody<JobSpec>,
) -> Result<(StatusCode, Json<Job>), ApiFailure> {
    let now = now_ms();
    let added = app
        .with_store(move |store, cfg| add_job_once(store, cfg, &spec, now))
        .await?;
    let status = match added {
        Added::New(_) => StatusCode::CREATED,
        Added::Existing(_) => StatusCode::OK,
    };
    Ok((status, Json(operator_job(added.into_job()))))
}

/// `POST /v1/jobs/{id}/cancel`: the job as stored after cancelling.
pub async fn cancel(
    State(app): State<AppState>,
    path: Result<Path<JobId>, PathRejection>,
) -> Result<Json<Job>, ApiFailure> {
    let Path(id) = path?;
    let req = ControlRequest {
        scope: ControlScope::Job(id),
        action: ControlAction::Cancel,
        reason: Some("cancelled via API".into()),
    };
    let now = now_ms();
    let job = app
        .with_store(move |store, _| {
            apply_control(store, &req, &[], now)?;
            store
                .get_job(id)?
                .ok_or_else(|| ApiFailure::NotFound(format!("job {id} not found")))
        })
        .await?;
    Ok(Json(operator_job(job)))
}

/// Stdin can contain private prompts; only agent leases may carry it.
pub(super) fn operator_job(mut job: Job) -> Job {
    job.spec.stdin = None;
    job
}

/// `POST /v1/control`: the cluster status after the change.
pub async fn control(
    State(app): State<AppState>,
    JsonBody(req): JsonBody<ControlRequest>,
) -> Result<Json<ClusterStatus>, ApiFailure> {
    let now = now_ms();
    let status = app
        .with_store(move |store, cfg| {
            let accounts: Vec<String> = cfg.quota.accounts.iter().map(|a| a.name.clone()).collect();
            apply_control(store, &req, &accounts, now)?;
            cluster_status(store, cfg, now)
        })
        .await?;
    Ok(Json(status))
}

/// `GET /v1/events?limit=`, newest first.
pub async fn events(
    State(app): State<AppState>,
    query: Result<Query<LimitQuery>, QueryRejection>,
) -> Result<Json<Vec<Event>>, ApiFailure> {
    let Query(q) = query?;
    let limit = page_limit(q.limit);
    let events = app.with_store(move |store, _| Ok::<_, ApiFailure>(store.events(limit)?));
    Ok(Json(events.await?))
}

/// `GET /v1/quota`.
pub async fn quota(State(app): State<AppState>) -> Result<Json<Vec<QuotaView>>, ApiFailure> {
    let now = now_ms();
    let views =
        app.with_store(move |store, cfg| Ok::<_, ApiFailure>(quota_views(store, cfg, now)?));
    Ok(Json(views.await?))
}

pub(super) fn add_job(
    store: &mut Store,
    cfg: &Config,
    spec: &JobSpec,
    now: UnixMs,
) -> Result<Job, ApiFailure> {
    Ok(add_job_once(store, cfg, spec, now)?.into_job())
}

/// Queues the spec unless its key already names an unfinished job (then nothing changes).
fn add_job_once(
    store: &mut Store,
    cfg: &Config,
    spec: &JobSpec,
    now: UnixMs,
) -> Result<Added, ApiFailure> {
    admit_job(spec, &cfg.quota)?;
    let added = store.insert_job_once(spec, now)?;
    if let Added::New(job) = &added {
        let msg = format!("{} queued (priority {})", job.spec.name, job.spec.priority);
        store.append_event(EventKind::JobQueued, None, Some(job.id), &msg, now)?;
    }
    Ok(added)
}

/// Persists the pause flag and changed job rows atomically.
pub(super) fn apply_control(
    store: &mut Store,
    req: &ControlRequest,
    accounts: &[String],
    now: UnixMs,
) -> Result<(), ApiFailure> {
    let mut jobs = store.active_jobs()?;
    if let ControlScope::Job(id) = req.scope {
        if !jobs.iter().any(|j| j.id == id) {
            jobs.extend(store.get_job(id)?);
        }
    }
    let plan = plan_control(req, &jobs, now)?;
    if matches!(
        req.scope,
        ControlScope::Device(_) | ControlScope::Account(_)
    ) {
        let devices: Vec<String> = store
            .devices(now, i64::MAX)?
            .into_iter()
            .map(|d| d.info.id)
            .collect();
        if let Some(why) = unknown_scope(req, &devices, accounts) {
            return Err(ApiFailure::BadRequest(why));
        }
    }
    store.apply_control(&plan)?;
    let (device, job) = match &req.scope {
        ControlScope::Device(d) => (Some(d), None),
        ControlScope::Job(id) => (None, Some(*id)),
        _ => (None, None),
    };
    store.append_event(EventKind::Control, device, job, &plan.summary, now)?;
    for after in &plan.jobs {
        if let Some(before) = jobs.iter().find(|j| j.id == after.id) {
            log_transition(store, before, after, now)?;
        }
    }
    Ok(())
}

pub(super) fn cluster_status(
    store: &Store,
    cfg: &Config,
    now: UnixMs,
) -> Result<ClusterStatus, ApiFailure> {
    let paused = store.paused_scopes()?;
    let open_advice = store
        .list_recommendations(0)?
        .iter()
        .filter(|r| r.status == RecommendationStatus::Open)
        .count();
    Ok(ClusterStatus {
        at_ms: now,
        version: VERSION.into(),
        global_paused: global_paused(&paused),
        paused_accounts: paused_accounts(&paused),
        devices: store.devices(now, offline_after_ms(cfg))?,
        jobs: store.job_counts()?,
        open_advice: u64::try_from(open_advice).unwrap_or(u64::MAX),
    })
}

pub(super) fn quota_views(
    store: &Store,
    cfg: &Config,
    now: UnixMs,
) -> Result<Vec<QuotaView>, StoreError> {
    let since = now.saturating_sub(QUOTA_LOOKBACK_MS);
    let snapshots = store.latest_quota()?;
    let interactive_use = store.interactive_use()?;
    let leased_accounts = store.ai_leased_accounts()?;
    let limit_hits = store.limit_hits(since)?;
    let ai_job_starts = store.ai_job_starts(since)?;
    let ctx = SpendContext {
        now_ms: now,
        snapshots: &snapshots,
        interactive_use: &interactive_use,
        leased_accounts: &leased_accounts,
        limit_hits: &limit_hits,
        ai_job_starts: &ai_job_starts,
    };
    Ok(quota::views(&cfg.quota, &ctx))
}
