//! Advice routes and advisor runs: list, refresh, accept, dismiss, and ingesting a finished
//! AI pass. Adapter: the advisor decides, this module persists and logs.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use idlemax_core::advice::{self, AdviceError};
use idlemax_core::control::admit_job;
use idlemax_core::protocol::ClusterStatus;
use idlemax_core::types::{EventKind, Job, JobId, Recommendation, UnixMs};
use idlemax_core::Config;

use super::http::ApiFailure;
use super::jobs::{cluster_status, operator_job, quota_views, LimitQuery};
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
    run_refresh_with(app, now, |state, existing| async move {
        advisor_adapter::refresh(app.paths(), &app.config().advisor, state, &existing, now)
            .await
            .map_err(|e| ApiFailure::Unavailable(format!("advisor: {e}")))
    })
    .await
}

async fn run_refresh_with<F, Fut>(app: &AppState, now: UnixMs, compute: F) -> Result<(), ApiFailure>
where
    F: FnOnce(CoordState, Vec<Recommendation>) -> Fut,
    Fut: std::future::Future<Output = Result<AdviceChanges, ApiFailure>>,
{
    let _refresh = app.refresh_lock.lock().await;
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
    let changes = compute(state, existing).await?;
    app.with_store(move |store, cfg| persist(store, cfg, &changes, "refresh", None, now))
        .await
}

/// Turns a finished AI pass into advice. Failures become Advice events; the update succeeds.
/// Every outcome records an Advice event naming `job`, which is how a replay of the update
/// knows the ingest finished.
pub async fn ingest_ai_output(app: &AppState, job: JobId, output: String, now: UnixMs) {
    let _refresh = app.refresh_lock.lock().await;
    let gathered = app
        .with_store(move |store, cfg| {
            Ok::<_, ApiFailure>((
                cluster_status(store, cfg, now)?,
                existing_advice(store, now)?,
            ))
        })
        .await;
    let result = match gathered {
        Ok((status, existing)) => ingest(app, job, &status, &existing, &output, now).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        let msg = format!("AI advice pass rejected: {e}");
        let logged = app
            .with_store(move |store, _| {
                Ok::<_, ApiFailure>(store.append_event(
                    EventKind::Advice,
                    None,
                    Some(job),
                    &msg,
                    now,
                )?)
            })
            .await;
        if let Err(log_err) = logged {
            tracing::warn!(error = %e, %log_err, "AI advice pass failed and could not be logged");
        }
    }
}

async fn ingest(
    app: &AppState,
    job: JobId,
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
    app.with_store(move |store, cfg| persist(store, cfg, &changes, "AI pass", Some(job), now))
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

/// Persists an advisor plan atomically. Failed admission leaves the plan unapplied.
/// The closing event names `job` (the AI pass) when there is one: it marks the plan as done.
fn persist(
    store: &mut Store,
    cfg: &Config,
    changes: &AdviceChanges,
    origin: &str,
    job: Option<JobId>,
    now: UnixMs,
) -> Result<(), ApiFailure> {
    for spec in &changes.queue {
        admit_job(spec, &cfg.quota)?;
    }
    store.persist_advice(changes, origin, job, now)?;
    Ok(())
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

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use idlemax_core::types::RecommendationStatus;

    #[tokio::test]
    async fn concurrent_refreshes_queue_one_mock_pass_and_preserve_mid_refresh_acceptance() {
        let tmp = tempfile::tempdir().unwrap();
        let mut store = Store::open_in_memory().unwrap();
        let id = store
            .insert_recommendation(&Recommendation::default())
            .unwrap();
        let app = AppState::new(
            store,
            Config::default(),
            idlemax_core::Paths::new(tmp.path()),
        );
        let changes = AdviceChanges {
            close: vec![id],
            queue: vec![idlemax_core::types::JobSpec {
                name: idlemax_core::advice::AI_PASS_JOB_NAME.into(),
                key: Some("advisor-ai-pass:fixture".into()),
                command: vec!["/bin/echo".into(), "mock pass".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let first_app = app.clone();
        let first_changes = changes.clone();
        let first = tokio::spawn(async move {
            run_refresh_with(&first_app, 1, |_, _| async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                Ok(first_changes)
            })
            .await
        });
        entered_rx.await.unwrap();
        app.with_store(move |store, _| {
            store.set_recommendation_status(id, RecommendationStatus::Accepted)?;
            Ok::<_, ApiFailure>(())
        })
        .await
        .unwrap();
        let second_app = app.clone();
        let (second_ready, second_ready_rx) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            second_ready.send(()).unwrap();
            run_refresh_with(&second_app, 2, |state, existing| async move {
                assert_eq!(
                    state.jobs.len(),
                    1,
                    "refresh gathered before the earlier plan committed"
                );
                assert_eq!(existing[0].status, RecommendationStatus::Accepted);
                Ok(changes)
            })
            .await
        });
        second_ready_rx.await.unwrap();
        release.send(()).unwrap();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        app.with_store(move |store, _| {
            assert_eq!(store.job_counts()?.queued, 1);
            let stale = store.queued_jobs()?.remove(0);
            store.persist_advice(
                &AdviceChanges {
                    cancel: vec![stale.id],
                    queue: vec![stale.spec],
                    ..Default::default()
                },
                "refresh",
                None,
                3,
            )?;
            assert_eq!(store.job_counts()?.queued, 1);
            assert_eq!(
                store.get_job(stale.id)?.unwrap().status,
                idlemax_core::types::JobStatus::Cancelled
            );
            assert_eq!(
                store.get_recommendation(id)?.unwrap().status,
                RecommendationStatus::Accepted
            );
            Ok::<_, ApiFailure>(())
        })
        .await
        .unwrap();
    }

    #[test]
    fn stale_close_preserves_accepted_advice_and_failed_queue_rolls_back() {
        let mut store = Store::open_in_memory().unwrap();
        let accepted = Recommendation {
            status: RecommendationStatus::Accepted,
            ..Default::default()
        };
        let id = store.insert_recommendation(&accepted).unwrap();
        let changes = AdviceChanges {
            close: vec![id],
            ..Default::default()
        };
        persist(&mut store, &Config::default(), &changes, "refresh", None, 1).unwrap();
        assert_eq!(
            store.get_recommendation(id).unwrap().unwrap().status,
            RecommendationStatus::Accepted
        );
        let changes = AdviceChanges {
            insert: vec![Recommendation {
                key: "must-rollback".into(),
                ..accepted
            }],
            queue: vec![idlemax_core::types::JobSpec::default()],
            ..Default::default()
        };
        assert!(persist(&mut store, &Config::default(), &changes, "refresh", None, 2).is_err());
        assert_eq!(store.list_recommendations(0).unwrap().len(), 1);
        let mut changes = changes;
        changes.queue.insert(
            0,
            idlemax_core::types::JobSpec {
                name: "must-rollback".into(),
                command: vec!["/bin/echo".into()],
                ..Default::default()
            },
        );
        assert!(store.persist_advice(&changes, "refresh", None, 3).is_err());
        assert_eq!(store.list_recommendations(0).unwrap().len(), 1);
        assert_eq!(store.job_counts().unwrap().queued, 0);
        assert_eq!(store.events(0).unwrap().len(), 1);
    }
}
