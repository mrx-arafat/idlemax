//! `GET /v1/jobs/{id}/why`. Adapter: gathers the scheduler's inputs from the store and asks
//! the domain `explain` function; it decides nothing itself.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::Json;
use idlemax_core::explain::{explain, ExplainInput, Explanation};
use idlemax_core::types::JobId;

use super::http::ApiFailure;
use super::jobs::quota_views;
use super::{offline_after_ms, AppState};
use crate::state::now_ms;

pub async fn why(
    State(app): State<AppState>,
    path: Result<Path<JobId>, PathRejection>,
) -> Result<Json<Explanation>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let explanation = app
        .with_store(move |store, cfg| {
            let job = store
                .get_job(id)?
                .ok_or_else(|| ApiFailure::NotFound(format!("job {id} not found")))?;
            // The scheduler's own input: every active job plus every queued one.
            let mut jobs = store.active_jobs()?;
            jobs.extend(store.queued_jobs()?);
            let devices = store.devices(now, offline_after_ms(cfg))?;
            let paused = store.paused_scopes()?;
            let quota = quota_views(store, cfg, now)?;
            Ok::<_, ApiFailure>(explain(&ExplainInput {
                now_ms: now,
                job: &job,
                jobs: &jobs,
                devices: &devices,
                paused: &paused,
                quota: &quota,
            }))
        })
        .await?;
    Ok(Json(explanation))
}
