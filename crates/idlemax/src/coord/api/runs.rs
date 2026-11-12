//! Run routes (adapter): read the Runs of the dispatcher's workers, and the owner's stop and
//! start of every worker. The `launch` column, which holds the brief, is never read here, so
//! it never leaves the store.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use idlemax_core::types::{ControlScope, EventKind};
use idlemax_core::worker::control::{
    plan_start, plan_stop, StartReply, StopError, StopReply, StopRequest,
};
use idlemax_core::worker::run::{Run, RunState};
use idlemax_core::worker::RunId;
use serde::Deserialize;

use super::http::{ApiFailure, JsonBody};
use super::{page_limit, AppState};
use crate::coord::dispatch::stop_workers;
use crate::state::now_ms;

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
    JsonBody(req): JsonBody<StopRequest>,
) -> Result<Json<StopReply>, ApiFailure> {
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
                ControlScope::Device(_) => return Err(bad(StopError::Scope("device"))),
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
pub async fn start(State(app): State<AppState>) -> Result<Json<StartReply>, ApiFailure> {
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

fn bad(e: StopError) -> ApiFailure {
    ApiFailure::BadRequest(e.to_string())
}

#[cfg(test)]
mod tests;
