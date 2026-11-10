//! `GET /v1/jobs/{id}/why` and `GET /v1/why`. Adapter: gathers the scheduler's inputs from
//! the store and asks the domain `explain` function; it decides nothing itself.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use idlemax_core::explain::{explain, summarize, ExplainInput, Explanation, WhySummary};
use idlemax_core::protocol::DeviceView;
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{ControlScope, Job, JobId, JobStatus, UnixMs};
use idlemax_core::Config;

use super::agents::scheduler_jobs_and_held;
use super::http::ApiFailure;
use super::jobs::{quota_views, LimitQuery};
use super::{offline_after_ms, page_limit, AppState};
use crate::coord::store::Store;
use crate::state::now_ms;

/// The coordinator's state at one instant, read once and shared by every explanation and by
/// the capacity shown in `GET /v1/status`.
pub(super) struct Snapshot {
    now: UnixMs,
    /// What the scheduler picks from: active jobs and queued jobs no pause holds.
    pub(super) jobs: Vec<Job>,
    /// Queued jobs a pause holds back; the scheduler never sees them.
    pub(super) held: Vec<Job>,
    pub(super) devices: Vec<DeviceView>,
    pub(super) paused: Vec<ControlScope>,
    pub(super) quota: Vec<QuotaView>,
}

impl Snapshot {
    pub(super) fn gather(store: &Store, cfg: &Config, now: UnixMs) -> Result<Self, ApiFailure> {
        let paused = store.paused_scopes()?;
        let (jobs, held) = scheduler_jobs_and_held(store, &paused)?;
        Ok(Self {
            now,
            jobs,
            held,
            devices: store.devices(now, offline_after_ms(cfg))?,
            quota: quota_views(store, cfg, now)?,
            paused,
        })
    }

    fn explain(&self, job: &Job) -> Explanation {
        explain(&ExplainInput {
            now_ms: self.now,
            job,
            jobs: &self.jobs,
            devices: &self.devices,
            paused: &self.paused,
            quota: &self.quota,
        })
    }
}

/// `GET /v1/jobs/{id}/why`: every reason one job is not running.
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
            Ok::<_, ApiFailure>(Snapshot::gather(store, cfg, now)?.explain(&job))
        })
        .await?;
    Ok(Json(explanation))
}

/// `GET /v1/why?limit=`: one headline reason per queued, leased or paused job, newest first.
/// The dashboard polls this once instead of asking per job.
pub async fn why_all(
    State(app): State<AppState>,
    query: Result<Query<LimitQuery>, QueryRejection>,
) -> Result<Json<Vec<WhySummary>>, ApiFailure> {
    let Query(q) = query?;
    let limit = usize::try_from(page_limit(q.limit)).unwrap_or(usize::MAX);
    let now = now_ms();
    let rows = app
        .with_store(move |store, cfg| {
            let snap = Snapshot::gather(store, cfg, now)?;
            let mut waiting: Vec<&Job> = snap
                .jobs
                .iter()
                .chain(&snap.held)
                .filter(|j| {
                    matches!(
                        j.status,
                        JobStatus::Queued | JobStatus::Leased | JobStatus::Paused
                    )
                })
                .collect();
            waiting.sort_by_key(|j| std::cmp::Reverse(j.id));
            waiting.truncate(limit);
            Ok::<_, ApiFailure>(
                waiting
                    .into_iter()
                    .map(|j| summarize(&snap.explain(j)))
                    .collect::<Vec<_>>(),
            )
        })
        .await?;
    Ok(Json(rows))
}
