//! Device routes: switch a device on or off for dispatcher work (D19, A23), and make or
//! revoke its own credential (A15).
//! Adapter: the domain plans the switch and names what it stops; these handlers persist,
//! stop and log. Every route is an owner action, so no pause, kill switch, guard or line
//! state holds it (A10).

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::Json;
use idlemax_core::control::caller::Access;
use idlemax_core::control::credential::{CredentialReply, RevokeReply};
use idlemax_core::control::device::{checks_to_rerun, plan_switch, stop_targets, SwitchReply};
use idlemax_core::protocol::{name_problem, ControlRequest, MAX_DEVICE_ID_CHARS};
use idlemax_core::types::{ControlAction, ControlScope, EventKind, UnixMs};

use super::caller::Who;
use super::http::{credential_digest, ApiFailure};
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
    let id = device_id(path)?;
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| switch_device(store, &id, on, now))
        .await?;
    Ok(Json(reply))
}

/// Stores the switch. Off ends the device's own dispatcher jobs; on runs again the base
/// checks of its projects that ended without a result, so `step` queues them anew. A switch
/// that changes nothing does nothing else.
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
    } else if !on && changed {
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

/// `POST /v1/devices/{id}/credential`: owner-only; a new credential for the id, known or
/// not, since a new server has never sent a heartbeat. It replaces an older one.
pub async fn credential(
    State(app): State<AppState>,
    who: Who,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<CredentialReply>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let id = device_id(path)?;
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| issue_credential(store, &id, now))
        .await?;
    Ok(Json(reply))
}

/// `DELETE /v1/devices/{id}/credential`: owner-only.
pub async fn revoke(
    State(app): State<AppState>,
    who: Who,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<RevokeReply>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let id = device_id(path)?;
    let now = now_ms();
    let reply = app
        .with_store(move |store, _| {
            let removed = store.delete_device_credential(&id)?;
            if removed {
                let msg = format!("device {id}: credential revoked");
                store.append_event(EventKind::Control, Some(&id), None, &msg, now)?;
            }
            Ok::<_, ApiFailure>(RevokeReply {
                device: id,
                removed,
            })
        })
        .await?;
    Ok(Json(reply))
}

/// Makes a credential, keeps its digest and logs an event that never names the credential.
pub(super) fn issue_credential(
    store: &mut Store,
    id: &str,
    now: UnixMs,
) -> Result<CredentialReply, ApiFailure> {
    let credential =
        crate::state::generate_token().map_err(|e| ApiFailure::Internal(e.to_string()))?;
    store.set_device_credential(id, &credential_digest(&credential), now)?;
    let device = id.to_string();
    let msg = format!("device {id}: new credential made");
    store.append_event(EventKind::Control, Some(&device), None, &msg, now)?;
    Ok(CredentialReply { device, credential })
}

fn device_id(path: Result<Path<String>, PathRejection>) -> Result<String, ApiFailure> {
    let Path(id) = path?;
    match name_problem("device id", &id, MAX_DEVICE_ID_CHARS) {
        Some(why) => Err(ApiFailure::BadRequest(why)),
        None => Ok(id),
    }
}

#[cfg(test)]
mod tests;
