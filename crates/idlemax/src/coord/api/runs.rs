//! Run routes (adapter): read the Runs of the dispatcher's workers, and the owner's stop and
//! start of every worker. The `launch` column, which holds the brief, is never read here, so
//! it never leaves the store.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use idlemax_core::control::caller::Access;
use idlemax_core::scheduler::lease_deadline;
use idlemax_core::types::{ActivePolicy, ControlScope, CostClass, EventKind, Job, JobSpec};
use idlemax_core::worker::control::{
    plan_start, plan_stop, StartReply, StopError, StopReply, StopRequest,
};
use idlemax_core::worker::run::{Run, RunState};
use idlemax_core::worker::RunId;
use idlemax_core::worker::{WorkerAction, WorkerSpec};
use serde::Deserialize;

use super::caller::Who;
use super::http::{ApiFailure, JsonBody};
use super::{page_limit, AppState};
use crate::coord::dispatch::stop_workers;
use crate::state::now_ms;

/// A review runs only git: a little memory and a few minutes at most.
const REVIEW_MEM_MB: u64 = 128;
const REVIEW_RUNTIME_SECS: u64 = 600;

#[derive(Debug, Deserialize)]
pub struct RunsQuery {
    state: Option<String>,
    project: Option<String>,
    limit: Option<u32>,
}

/// `GET /v1/runs?state=&project=&limit=`, newest first.
pub async fn list(
    State(app): State<AppState>,
    query: Result<Query<RunsQuery>, QueryRejection>,
) -> Result<Json<Vec<Run>>, ApiFailure> {
    let Query(q) = query?;
    let state = match q.state.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => Some(s.parse::<RunState>().map_err(ApiFailure::BadRequest)?),
    };
    let project = q.project.filter(|p| !p.trim().is_empty());
    let limit = page_limit(q.limit);
    let runs = app
        .with_store(move |store, _| {
            Ok::<_, ApiFailure>(store.list_runs(state, project.as_deref(), limit)?)
        })
        .await?;
    Ok(Json(runs))
}

/// `GET /v1/runs/{id}`.
pub async fn get_one(
    State(app): State<AppState>,
    path: Result<Path<RunId>, PathRejection>,
) -> Result<Json<Run>, ApiFailure> {
    let Path(id) = path?;
    let run = app
        .with_store(move |store, _| {
            store
                .get_run(id)?
                .ok_or_else(|| ApiFailure::NotFound(format!("run {id} not found")))
        })
        .await?;
    Ok(Json(run))
}

/// `POST /v1/workers/stop`: the owner's global stop, or an account's. It is one store write,
/// so no line, guard or pause holds it (A10).
pub async fn stop(
    State(app): State<AppState>,
    who: Who,
    JsonBody(req): JsonBody<StopRequest>,
) -> Result<Json<StopReply>, ApiFailure> {
    who.ensure(Access::Stop).await?;
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| {
            let why = req
                .reason
                .clone()
                .unwrap_or_else(|| "stopped by you".into());
            let done = match &req.scope {
                ControlScope::Global => {
                    let runs = store.list_runs(None, None, 0)?;
                    let moved = plan_stop(&runs, &req.scope, now).map_err(bad)?;
                    let done = store.owner_stop(&moved, &why, now)?;
                    let msg = format!("workers stopped ({} live): {why}", done.len());
                    store.append_event(EventKind::Control, None, None, &msg, now)?;
                    done
                }
                ControlScope::Account(_) => stop_workers(store, &req.scope, &why, now)?,
                // A stop without the switch would be undone by the next sweep, which
                // relaunches shed Runs.
                ControlScope::Device(d) => {
                    return Err(ApiFailure::BadRequest(format!(
                        "use POST /v1/devices/{d}/off to stop a device's workers"
                    )))
                }
                ControlScope::Job(_) => return Err(bad(StopError::Scope("job"))),
            };
            Ok::<_, ApiFailure>(StopReply {
                workers: done.iter().map(|r| r.id).collect(),
                stop_in_force: store.worker_stop()?.is_some(),
            })
        })
        .await?;
    Ok(Json(reply))
}

/// `POST /v1/workers/start`: lifts the owner's stop and puts every stopped Run back in the
/// queue, which the launch step drains one at a time (D11). The reply names every pause
/// still in force.
pub async fn start(State(app): State<AppState>, who: Who) -> Result<Json<StartReply>, ApiFailure> {
    // It lifts the owner's stop, so a launched job can never call it (A14).
    who.ensure(Access::Owner).await?;
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| {
            let runs = store.list_runs(None, None, 0)?;
            let done = store.owner_start(&plan_start(&runs, now))?;
            let msg = format!("workers started: {} go back in the queue", done.len());
            store.append_event(EventKind::Control, None, None, &msg, now)?;
            Ok::<_, ApiFailure>(StartReply {
                workers: done.iter().map(|r| r.id).collect(),
                paused: store.paused_scopes()?,
            })
        })
        .await?;
    Ok(Json(reply))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct KeepRequest {
    merge: bool,
}

/// `POST /v1/runs/{id}/keep` with `{"merge": bool}`: a keep or keep-and-merge job leased to
/// the Run's device. 404 for an unknown Run, 409 for one with a live job or not finished.
pub async fn keep(
    state: State<AppState>,
    who: Who,
    path: Result<Path<RunId>, PathRejection>,
    JsonBody(req): JsonBody<KeepRequest>,
) -> Result<(StatusCode, Json<Job>), ApiFailure> {
    who.ensure(Access::Owner).await?;
    let action = if req.merge {
        WorkerAction::KeepMerge
    } else {
        WorkerAction::Keep
    };
    review(state, path, action).await
}

/// `POST /v1/runs/{id}/discard`: a discard job leased to the Run's device.
pub async fn discard(
    state: State<AppState>,
    who: Who,
    path: Result<Path<RunId>, PathRejection>,
) -> Result<(StatusCode, Json<Job>), ApiFailure> {
    who.ensure(Access::Owner).await?;
    review(state, path, WorkerAction::Discard).await
}

async fn review(
    State(app): State<AppState>,
    path: Result<Path<RunId>, PathRejection>,
    action: WorkerAction,
) -> Result<(StatusCode, Json<Job>), ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let job = app
        .with_store(move |store, cfg| {
            let run = store.get_run(id)?.ok_or_else(|| {
                ApiFailure::NotFound(format!("run {id} not found; idlemax review lists the runs"))
            })?;
            reviewable(store, &run, action)?;
            let launch = store.run_launch(id)?.unwrap_or_default();
            let spec = review_spec(&run, launch.worker, action);
            let expires = lease_deadline(cfg.coordinator.lease_secs, now);
            let job = store
                .start_review(&run, &spec, expires, now)?
                .ok_or_else(|| ApiFailure::Conflict(format!("run {id} changed; try again")))?;
            let msg = format!("{} of run {id} sent to {}", action_word(action), run.device);
            store.append_event(
                EventKind::Control,
                Some(&run.device),
                Some(job.id),
                &msg,
                now,
            )?;
            Ok::<_, ApiFailure>(job)
        })
        .await?;
    Ok((StatusCode::CREATED, Json(super::jobs::operator_job(job))))
}

/// 409 unless the Run may take this action now (D16).
fn reviewable(
    store: &crate::coord::store::Store,
    run: &Run,
    action: WorkerAction,
) -> Result<(), ApiFailure> {
    let live = match run.job {
        Some(job) => store.get_job(job)?.is_some_and(|j| !j.status.is_terminal()),
        None => false,
    };
    if live {
        return Err(ApiFailure::Conflict(format!(
            "run {} has a live job; wait for it, or stop it with idlemax stop",
            run.id
        )));
    }
    let allowed = match action {
        WorkerAction::Discard => matches!(
            run.state,
            RunState::Clean
                | RunState::NeedsYou
                | RunState::Rejected
                | RunState::Stopped
                | RunState::Shed
        ),
        _ => matches!(
            run.state,
            RunState::Clean | RunState::NeedsYou | RunState::Rejected
        ),
    };
    match allowed {
        true => Ok(()),
        false => Err(ApiFailure::Conflict(format!(
            "run {} is {}; it cannot be {} now",
            run.id,
            run.state,
            action_word(action)
        ))),
    }
}

fn review_spec(run: &Run, saved: WorkerSpec, action: WorkerAction) -> JobSpec {
    let worker = WorkerSpec {
        run: run.id,
        action,
        branch: run.branch.clone(),
        worktree: run.worktree.clone(),
        ..saved
    };
    JobSpec {
        name: format!("review-{}", run.id),
        key: Some(format!("run-{}", run.id)),
        command: vec!["idlemax-worker".into(), "git".into()],
        devices: vec![run.device.clone()],
        cost: CostClass::Ai,
        ai_account: Some(run.account.clone()),
        mem_mb: REVIEW_MEM_MB,
        max_runtime_secs: REVIEW_RUNTIME_SECS,
        on_user_active: ActivePolicy::Continue,
        max_attempts: 1,
        worker: Some(worker),
        ..Default::default()
    }
}

fn action_word(action: WorkerAction) -> &'static str {
    match action {
        WorkerAction::Discard => "discarded",
        WorkerAction::KeepMerge => "kept and merged",
        _ => "kept",
    }
}

fn bad(e: StopError) -> ApiFailure {
    ApiFailure::BadRequest(e.to_string())
}

#[cfg(test)]
mod tests;
