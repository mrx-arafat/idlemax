//! Dispatcher routes: the plan and the agents' dispatch feed. Adapter: stores the feed, turns it into
//! candidate projects and owner activity, and replies with the device's switched-on projects.

use axum::extract::State;
use axum::{Extension, Json};
use idlemax_core::control::caller::Access;
use idlemax_core::control::credential::Identity;
use idlemax_core::control::{device_gate, may_assign};
use idlemax_core::dispatch::inputs::{DispatchInputs, DispatchReply};
use idlemax_core::dispatch::plan::Plan;
use idlemax_core::dispatch::Mode;
use idlemax_core::types::{JobId, UnixMs};
use idlemax_core::Config;

use super::caller::Who;
use super::http::{ApiFailure, JsonBody};
use super::offline_after_ms;
use super::AppState;
use crate::coord::dispatch::plan_with_holds;
use crate::coord::store::Store;
use crate::state::now_ms;

/// `GET /v1/plan`: what the dispatcher would launch now and why; it launches nothing.
pub async fn plan(State(app): State<AppState>) -> Result<Json<Plan>, ApiFailure> {
    let now = now_ms();
    let live = std::sync::Arc::clone(app.live());
    let gathered = app
        .with_store(move |store, cfg| {
            Ok::<_, ApiFailure>(plan_with_holds(store, cfg, &live.current(), now)?)
        })
        .await?;
    Ok(Json(gathered.plan))
}

/// `POST /v1/plan/refresh`: runs the dispatcher step with a pass asked for, and returns the
/// plan. It launches nothing, so no pause holds it; the pass job obeys every pause and the
/// lines. It spends quota, so it is owner-only (Spec 4 Task 4).
pub async fn refresh(State(app): State<AppState>, who: Who) -> Result<Json<Plan>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let _serial = app.dispatch_lock().lock().await;
    let now = now_ms();
    let live = std::sync::Arc::clone(app.live());
    let paths = app.paths().clone();
    let gathered = app
        .with_store(move |store, cfg| {
            let live = live.current();
            let rules = crate::dispatch::rules_file::read(&paths);
            crate::dispatch::step(store, cfg, &live, &rules, now, true)?;
            Ok::<_, ApiFailure>(plan_with_holds(store, cfg, &live, now)?)
        })
        .await?;
    Ok(Json(gathered.plan))
}

/// The answer of a finished orchestrator pass, checked and stored.
pub async fn ingest_pass(app: &AppState, job: JobId, output: String, now: UnixMs) {
    let live = std::sync::Arc::clone(app.live());
    let paths = app.paths().clone();
    let done = app
        .with_store(move |store, cfg| {
            let rules = crate::dispatch::rules_file::read(&paths);
            crate::dispatch::ingest_pass(store, cfg, &live.current(), &rules, job, &output, now)
                .map_err(ApiFailure::from)
        })
        .await;
    if let Err(e) = done {
        tracing::warn!(error = %e, job, "orchestrator pass ingest failed");
    }
}

/// `POST /v1/agent/dispatch-inputs`.
pub async fn dispatch_inputs(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    JsonBody(inputs): JsonBody<DispatchInputs>,
) -> Result<Json<DispatchReply>, ApiFailure> {
    super::agents::own_device(&identity, &inputs.device)?;
    if inputs.device.trim().is_empty() {
        return Err(ApiFailure::BadRequest("device is empty".into()));
    }
    inputs.check().map_err(ApiFailure::BadRequest)?;
    let now = now_ms();
    let live = std::sync::Arc::clone(app.live());
    let reply = app
        .with_store(move |store, cfg| ingest_feed(store, cfg, &live.current(), &inputs, now))
        .await?;
    Ok(Json(reply))
}

/// Stores the feed and its projects, then lists the device's switched-on projects; none
/// while the mode is `off`, the device is switched off or may not take work, so one gate stops every project
/// action on the device.
pub(super) fn ingest_feed(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    inputs: &DispatchInputs,
    now: UnixMs,
) -> Result<DispatchReply, ApiFailure> {
    store.put_dispatch_inputs(inputs, now)?;
    store.upsert_discovered(&inputs.device, &inputs.repos, now)?;
    if live.dispatcher.mode == Mode::Off {
        return Ok(DispatchReply::default());
    }
    let device = store
        .devices(now, offline_after_ms(cfg))?
        .into_iter()
        .find(|d| d.info.id == inputs.device);
    let Some(device) = device.filter(|d| device_gate(d).is_none()) else {
        return Ok(DispatchReply::default());
    };
    if !may_assign(&store.paused_scopes()?, &inputs.device, device.local_paused) {
        return Ok(DispatchReply::default());
    }
    let projects = store
        .list_projects(Some(&inputs.device))?
        .into_iter()
        .filter(|v| v.project.enabled)
        .map(|v| v.project)
        .collect();
    Ok(DispatchReply { projects })
}

#[cfg(test)]
mod recheck_tests;
#[cfg(test)]
pub(crate) mod tests;
