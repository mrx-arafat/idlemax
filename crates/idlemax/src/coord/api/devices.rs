//! Device routes: switch a device on or off for dispatcher work (D19, A23).
//! Adapter: the domain plans the switch and names what it stops; these handlers persist,
//! stop and log. Both are owner actions, so no pause, kill switch, guard or line state
//! holds them (A10).

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::Json;
use idlemax_core::control::caller::Access;
use idlemax_core::control::device::{checks_to_rerun, plan_switch, stop_targets, SwitchReply};
use idlemax_core::protocol::{name_problem, ControlRequest, MAX_DEVICE_ID_CHARS};
use idlemax_core::types::{ControlAction, ControlScope, UnixMs};

use super::caller::Who;
use super::http::ApiFailure;
use super::jobs::apply_control;
use super::AppState;
use crate::coord::store::Store;
use crate::state::now_ms;

/// `POST /v1/devices/{id}/on`: owner-only.
pub async fn on(
    state: State<AppState>,
    who: Who,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<SwitchReply>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    switch(state, path, true).await
}

/// `POST /v1/devices/{id}/off`: `Stop` access, so any holder of the token.
pub async fn off(
    state: State<AppState>,
    who: Who,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<SwitchReply>, ApiFailure> {
    who.ensure(Access::Stop).await?;
    switch(state, path, false).await
}

async fn switch(
    State(app): State<AppState>,
    path: Result<Path<String>, PathRejection>,
    on: bool,
) -> Result<Json<SwitchReply>, ApiFailure> {
    let Path(id) = path?;
    if let Some(why) = name_problem("device id", &id, MAX_DEVICE_ID_CHARS) {
        return Err(ApiFailure::BadRequest(why));
    }
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| switch_device(store, &id, on, now))
        .await?;
    Ok(Json(reply))
}

/// Stores the switch. Off ends the device's own dispatcher jobs; on runs again the base
/// checks of its projects that ended without a result, so `step` queues them anew.
// SHORTCUT: runs_stopped stays 0 until Spec 2's stop_workers lands, then off sheds the
// device's Runs here with their state saved.
pub(super) fn switch_device(
    store: &mut Store,
    id: &str,
    on: bool,
    now: UnixMs,
) -> Result<SwitchReply, ApiFailure> {
    let devices = store.devices(now, i64::MAX)?;
    let known: Vec<String> = devices.iter().map(|d| d.info.id.clone()).collect();
    let current = devices
        .iter()
        .find(|d| d.info.id == id)
        .and_then(|d| d.enabled);
    let changed = plan_switch(&known, current, id, on)?.changed();
    store.set_device_dispatch(id, on, now)?;
    let mut jobs_stopped = 0;
    if on && changed {
        let projects: Vec<i64> = store
            .list_projects(Some(id))?
            .into_iter()
            .map(|v| v.project.id)
            .collect();
        for project in checks_to_rerun(&store.base_checks()?, &projects) {
            store.delete_base_check(project)?;
        }
    } else if !on {
        let mut jobs = store.active_jobs()?;
        jobs.extend(store.queued_jobs()?);
        for job in stop_targets(&jobs, id) {
            let req = ControlRequest {
                scope: ControlScope::Job(job),
                action: ControlAction::StopAll,
                reason: Some(format!("device {id} switched off")),
            };
            apply_control(store, &req, &[], now)?;
            jobs_stopped += 1;
        }
    }
    Ok(SwitchReply {
        device: id.to_string(),
        enabled: on,
        changed,
        runs_stopped: 0,
        jobs_stopped,
    })
}

#[cfg(test)]
mod tests;
