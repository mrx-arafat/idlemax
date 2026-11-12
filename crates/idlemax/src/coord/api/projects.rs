//! Project routes: list, add, switch on or off and change the settings of a project.
//! Adapter: the domain checks names and settings; these handlers persist and log. Every route
//! is an owner action, so no pause, kill switch, guard or line state holds it (A10).

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use idlemax_core::dispatch::project::{
    owner_active, NewProject, ProjectSettings, ProjectSource, ProjectView,
};
use idlemax_core::explain::allowlist_warning;
use idlemax_core::types::{EventKind, JobSpec, UnixMs};
use idlemax_core::Config;
use serde::Deserialize;

use super::http::{ApiFailure, JsonBody};
use super::{offline_after_ms, AppState};
use crate::coord::store::Store;
use crate::state::now_ms;

#[derive(Debug, Deserialize)]
pub struct ProjectsQuery {
    device: Option<String>,
}

/// `GET /v1/projects?device=`, by device then name.
pub async fn list(
    State(app): State<AppState>,
    query: Result<Query<ProjectsQuery>, QueryRejection>,
) -> Result<Json<Vec<ProjectView>>, ApiFailure> {
    let Query(q) = query?;
    let now = now_ms();
    let views = app
        .with_store(move |store, _| project_views(store, q.device.as_deref(), now))
        .await?;
    Ok(Json(views))
}

/// `POST /v1/projects`: 201 with a new project, which is off, or 200 with the known one.
pub async fn add(
    State(app): State<AppState>,
    JsonBody(new): JsonBody<NewProject>,
) -> Result<(StatusCode, Json<ProjectView>), ApiFailure> {
    let now = now_ms();
    let (view, created) = app
        .with_store(move |store, cfg| {
            new.check().map_err(ApiFailure::BadRequest)?;
            let device = match &new.device {
                Some(d) => d.clone(),
                None => only_device(store, cfg, now)?,
            };
            let (view, created) = store.insert_project(&new, &device, ProjectSource::Owner, now)?;
            if created {
                log(store, &view, "added", now)?;
            }
            Ok::<_, ApiFailure>((view, created))
        })
        .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(view)))
}

/// `POST /v1/projects/{id}/on`.
pub async fn on(
    state: State<AppState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Json<ProjectView>, ApiFailure> {
    switch(state, path, true).await
}

/// `POST /v1/projects/{id}/off`.
pub async fn off(
    state: State<AppState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Json<ProjectView>, ApiFailure> {
    switch(state, path, false).await
}

async fn switch(
    State(app): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
    on: bool,
) -> Result<Json<ProjectView>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let view = app
        .with_store(move |store, _| {
            let before = existing(store, id, now)?;
            let after = store
                .set_project_enabled(id, on)?
                .ok_or_else(|| not_found(id))?;
            if before.project.enabled != on {
                after_switch(store, id, on)?;
                let what = if on { "switched on" } else { "switched off" };
                log(store, &after, what, now)?;
            }
            existing(store, id, now)
        })
        .await?;
    Ok(Json(view))
}

/// `POST /v1/projects/{id}/settings`: the whole settings, replaced.
pub async fn settings(
    State(app): State<AppState>,
    path: Result<Path<i64>, PathRejection>,
    JsonBody(settings): JsonBody<ProjectSettings>,
) -> Result<Json<ProjectView>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let view = app
        .with_store(move |store, cfg| {
            existing(store, id, now)?;
            settings.validate().map_err(ApiFailure::BadRequest)?;
            store.set_project_settings(id, &settings)?;
            after_settings(store, id)?;
            let mut view = existing(store, id, now)?;
            log(store, &view, "settings changed", now)?;
            view.warning = settings_warning(store, cfg, &view, now)?;
            Ok::<_, ApiFailure>(view)
        })
        .await?;
    Ok(Json(view))
}

/// Switching a project off drops its base check, so switching it on starts fresh. The
/// checkout folder stays.
fn after_switch(store: &mut Store, id: i64, on: bool) -> Result<(), ApiFailure> {
    if !on {
        store.delete_base_check(id)?;
    }
    Ok(())
}

/// New settings may change the check, so its record starts over.
fn after_settings(store: &mut Store, id: i64) -> Result<(), ApiFailure> {
    store.delete_base_check(id)?;
    Ok(())
}

/// Every project view, with `owner_active` filled from the feed and the session metadata.
pub(super) fn project_views(
    store: &Store,
    device: Option<&str>,
    now: UnixMs,
) -> Result<Vec<ProjectView>, ApiFailure> {
    let sessions: Vec<_> = store
        .advisor_inputs()?
        .into_iter()
        .flat_map(|s| {
            let device = s.inputs.device;
            s.inputs
                .sessions
                .into_iter()
                .map(move |m| (device.clone(), m))
        })
        .collect();
    let mut views = store.list_projects(device)?;
    for v in &mut views {
        v.owner_active = owner_active(v, &sessions, now);
    }
    Ok(views)
}

fn existing(store: &Store, id: i64, now: UnixMs) -> Result<ProjectView, ApiFailure> {
    let view = store.get_project(id)?.ok_or_else(|| not_found(id))?;
    let all = project_views(store, Some(&view.project.device), now)?;
    Ok(all.into_iter().find(|v| v.project.id == id).unwrap_or(view))
}

fn not_found(id: i64) -> ApiFailure {
    ApiFailure::NotFound(format!(
        "project {id} not found; see `idlemax project list`"
    ))
}

/// The one device the coordinator knows, or a 400 that names the known ones.
fn only_device(store: &Store, cfg: &Config, now: UnixMs) -> Result<String, ApiFailure> {
    let ids: Vec<String> = store
        .devices(now, offline_after_ms(cfg))?
        .into_iter()
        .map(|d| d.info.id)
        .collect();
    match ids.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(ApiFailure::BadRequest(
            "no device is known yet; start the agent, or pass the device".into(),
        )),
        many => Err(ApiFailure::BadRequest(format!(
            "pass the device, one of: {}",
            many.join(", ")
        ))),
    }
}

/// Says when the device's allowlist would refuse the setup or the check command.
fn settings_warning(
    store: &Store,
    cfg: &Config,
    view: &ProjectView,
    now: UnixMs,
) -> Result<Option<String>, ApiFailure> {
    let infos: Vec<_> = store
        .devices(now, offline_after_ms(cfg))?
        .into_iter()
        .map(|d| d.info)
        .collect();
    let s = &view.project.settings;
    let warnings: Vec<String> = [&s.check_command, &s.setup_command]
        .into_iter()
        .filter(|argv| !argv.is_empty())
        .filter_map(|argv| {
            let spec = JobSpec {
                name: "project".into(),
                command: argv.clone(),
                devices: vec![view.project.device.clone()],
                ..JobSpec::default()
            };
            allowlist_warning(&spec, &infos)
        })
        .collect();
    Ok((!warnings.is_empty()).then(|| warnings.join(" ")))
}

fn log(store: &mut Store, v: &ProjectView, what: &str, now: UnixMs) -> Result<(), ApiFailure> {
    let p = &v.project;
    let msg = format!("project {} on {} {what}", p.name, p.device);
    store.append_event(EventKind::Control, Some(&p.device), None, &msg, now)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use idlemax_core::protocol::ControlRequest;
    use idlemax_core::types::{ControlAction, ControlScope};
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;

    #[tokio::test]
    async fn a_switch_works_while_paused_and_logs_one_control_event() {
        let api = Api::start().await;
        api.heartbeat(Vec::new()).await;
        let res = api.post("/v1/projects/99/on", &()).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let new = NewProject {
            path: "/home/me/app".into(),
            ..Default::default()
        };
        let res = api.post("/v1/projects", &new).await;
        assert_eq!(res.status(), StatusCode::CREATED);
        let added: ProjectView = res.json().await.unwrap();
        assert_eq!(
            (added.project.device.as_str(), added.project.enabled),
            ("mac", false)
        );
        let again = api.post("/v1/projects", &new).await;
        assert_eq!(again.status(), StatusCode::OK);

        let pause = ControlRequest {
            action: ControlAction::Pause,
            scope: ControlScope::Global,
            reason: None,
        };
        assert_eq!(
            api.post("/v1/control", &pause).await.status(),
            StatusCode::OK
        );
        let id = added.project.id;
        let on: ProjectView = api
            .post(&format!("/v1/projects/{id}/on"), &())
            .await
            .json()
            .await
            .unwrap();
        assert!(on.project.enabled);
        let events: Vec<serde_json::Value> = api
            .get_as("/v1/events", "localhost")
            .await
            .json()
            .await
            .unwrap();
        let switched = events
            .iter()
            .filter(|e| e["message"] == "project app on mac switched on")
            .count();
        assert_eq!(switched, 1);
    }
}
