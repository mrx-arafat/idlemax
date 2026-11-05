//! Advice routes and advisor runs: list, refresh, accept, dismiss, and ingesting a finished
//! AI pass. Adapter: the advisor decides, this module persists and logs.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use idlemax_core::advice::{self, AdviceError};
use idlemax_core::control::admit_job;
use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::types::{
    ControlAction, ControlScope, EventKind, Job, JobId, JobStatus, Recommendation,
    RecommendationStatus, UnixMs,
};
use idlemax_core::Config;

use super::http::ApiFailure;
use super::jobs::{add_job, apply_control, cluster_status, operator_job, quota_views, LimitQuery};
use super::{page_limit, AppState};
use crate::advisor::{self as advisor_adapter, AdviceChanges, CoordState};
use crate::coord::store::Store;
use crate::state::now_ms;

/// The advisor dedups against advice of the last week.
const EXISTING_WINDOW_MS: i64 = 7 * 24 * 3_600_000;
/// Recent jobs the advisor looks at.
const RECENT_JOBS: u32 = 500;

/// `GET /v1/advice?limit=`, open first, then newest.
pub async fn list(
    State(app): State<AppState>,
    query: Result<Query<LimitQuery>, QueryRejection>,
) -> Result<Json<Vec<Recommendation>>, ApiFailure> {
    let Query(q) = query?;
    let limit = q.limit;
    let recs = app.with_store(move |store, _| {
        Ok::<_, ApiFailure>(store.list_recommendations(page_limit(limit))?)
    });
    Ok(Json(recs.await?))
}

/// `POST /v1/advice/refresh`: re-runs the advisor now, then lists advice.
pub async fn refresh(State(app): State<AppState>) -> Result<Json<Vec<Recommendation>>, ApiFailure> {
    run_refresh(&app, now_ms()).await?;
    let recs = app
        .with_store(|store, _| Ok::<_, ApiFailure>(store.list_recommendations(page_limit(None))?));
    Ok(Json(recs.await?))
}

/// `POST /v1/advice/{id}/accept`: queues the suggested job and returns it.
pub async fn accept(
    State(app): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Json<Job>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    Ok(Json(operator_job(
        app.with_store(move |store, cfg| accept_advice(store, cfg, id, now))
            .await?,
    )))
}

/// `POST /v1/advice/{id}/dismiss`.
pub async fn dismiss(
    State(app): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Json<Recommendation>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    Ok(Json(
        app.with_store(move |store, _| dismiss_advice(store, id, now))
            .await?,
    ))
}

/// One advisor run: gather state and the agents' inputs, let the advisor decide, persist.
pub async fn run_refresh(app: &AppState, now: UnixMs) -> Result<(), ApiFailure> {
    let (state, existing) = app
        .with_store(move |store, cfg| {
            let status = cluster_status(store, cfg, now)?;
            let state = CoordState {
                status,
                jobs: store.list_jobs(None, RECENT_JOBS)?,
                quota: quota_views(store, cfg, now)?,
                inputs: store.advisor_inputs()?,
            };
            Ok::<_, ApiFailure>((state, existing_advice(store, now)?))
        })
        .await?;
    let cfg = &app.config().advisor;
    let changes = advisor_adapter::refresh(app.paths(), cfg, state, &existing, now)
        .await
        .map_err(|e| ApiFailure::Unavailable(format!("advisor: {e}")))?;
    app.with_store(move |store, cfg| persist(store, cfg, &changes, "refresh", now))
        .await
}

/// Turns a finished AI pass into advice. Failures become Advice events; the update succeeds.
pub async fn ingest_ai_output(app: &AppState, output: String, now: UnixMs) {
    let gathered = app
        .with_store(move |store, cfg| {
            Ok::<_, ApiFailure>((
                cluster_status(store, cfg, now)?,
                existing_advice(store, now)?,
            ))
        })
        .await;
    let result = match gathered {
        Ok((status, existing)) => ingest(app, &status, &existing, &output, now).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        let msg = format!("AI advice pass rejected: {e}");
        let logged = app
            .with_store(move |store, _| {
                Ok::<_, ApiFailure>(store.append_event(EventKind::Advice, None, None, &msg, now)?)
            })
            .await;
        if let Err(log_err) = logged {
            tracing::warn!(error = %e, %log_err, "AI advice pass failed and could not be logged");
        }
    }
}

async fn ingest(
    app: &AppState,
    status: &ClusterStatus,
    existing: &[Recommendation],
    output: &str,
    now: UnixMs,
) -> Result<(), ApiFailure> {
    let cfg = &app.config().advisor;
    let changes =
        advisor_adapter::ingest_ai_output(app.paths(), cfg, status, existing, output, now)
            .await
            .map_err(|e| ApiFailure::BadRequest(e.to_string()))?;
    app.with_store(move |store, cfg| persist(store, cfg, &changes, "AI pass", now))
        .await
}

fn accept_advice(store: &mut Store, cfg: &Config, id: i64, now: UnixMs) -> Result<Job, ApiFailure> {
    let rec = store.get_recommendation(id)?.ok_or_else(|| not_found(id))?;
    let (_, spec) = advice::accept(&rec).map_err(advice_failure)?;
    admit_job(&spec, &cfg.quota)?;
    let job = store.accept_advice(id, &spec, now)?;
    let queued = format!("{} queued (priority {})", job.spec.name, job.spec.priority);
    store.append_event(EventKind::JobQueued, None, Some(job.id), &queued, now)?;
    let msg = format!(
        "accepted advice {id} ({}): job {} queued",
        rec.title, job.id
    );
    store.append_event(EventKind::Advice, None, Some(job.id), &msg, now)?;
    Ok(job)
}

fn dismiss_advice(store: &mut Store, id: i64, now: UnixMs) -> Result<Recommendation, ApiFailure> {
    let rec = store.get_recommendation(id)?.ok_or_else(|| not_found(id))?;
    let dismissed = advice::dismiss(&rec).map_err(advice_failure)?;
    let stored = store
        .set_recommendation_status(id, dismissed.status)?
        .ok_or_else(|| not_found(id))?;
    let msg = format!("dismissed advice {id} ({})", rec.title);
    store.append_event(EventKind::Advice, None, None, &msg, now)?;
    Ok(stored)
}

/// Persists an advisor plan. A queued spec that fails admission is logged, not fatal.
fn persist(
    store: &mut Store,
    cfg: &Config,
    changes: &AdviceChanges,
    origin: &str,
    now: UnixMs,
) -> Result<(), ApiFailure> {
    for id in &changes.close {
        store.set_recommendation_status(*id, RecommendationStatus::Dismissed)?;
    }
    for rec in &changes.insert {
        store.insert_recommendation(rec)?;
    }
    let mut queued = 0;
    for spec in &changes.queue {
        match add_job(store, cfg, spec, now) {
            Ok(_) => queued += 1,
            Err(ApiFailure::BadRequest(why)) => {
                let msg = format!("advisor job {} not queued: {why}", spec.name);
                store.append_event(EventKind::Advice, None, None, &msg, now)?;
            }
            Err(e) => return Err(e),
        }
    }
    let cancelled = cancel_stale(store, &changes.cancel, now)?;
    let msg = format!(
        "advice {origin}: {} new, {} closed, {queued} job(s) queued, {cancelled} stale cancelled",
        changes.insert.len(),
        changes.close.len()
    );
    store.append_event(EventKind::Advice, None, None, &msg, now)?;
    Ok(())
}

/// Cancels stale advisor jobs that are still queued; one leased since the advisor looked
/// keeps running, since killing it would waste the quota it already spends.
fn cancel_stale(store: &mut Store, ids: &[JobId], now: UnixMs) -> Result<usize, ApiFailure> {
    let mut cancelled = 0;
    for &id in ids {
        if store
            .get_job(id)?
            .is_some_and(|j| j.status == JobStatus::Queued)
        {
            let req = ControlRequest {
                scope: ControlScope::Job(id),
                action: ControlAction::Cancel,
                reason: Some("stale advisor AI pass".into()),
            };
            apply_control(store, &req, now)?;
            cancelled += 1;
        }
    }
    Ok(cancelled)
}

fn existing_advice(store: &Store, now: UnixMs) -> Result<Vec<Recommendation>, ApiFailure> {
    let since = now.saturating_sub(EXISTING_WINDOW_MS);
    let mut recs = store.list_recommendations(0)?;
    recs.retain(|r| r.at_ms >= since);
    Ok(recs)
}

fn not_found(id: i64) -> ApiFailure {
    ApiFailure::NotFound(format!("advice {id} not found"))
}

fn advice_failure(e: AdviceError) -> ApiFailure {
    match e {
        AdviceError::NotOpen { .. } | AdviceError::NoSuggestedJob(_) => {
            ApiFailure::Conflict(e.to_string())
        }
        AdviceError::InvalidJob(_) | AdviceError::AiOutput(_) => {
            ApiFailure::BadRequest(e.to_string())
        }
    }
}
