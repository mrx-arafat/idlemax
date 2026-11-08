//! Schedule routes: list, add, remove, pause and resume recurring jobs.
//! Adapter: the domain decides what is due; these handlers persist and report.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use idlemax_core::control::admit_job;
use idlemax_core::schedule::{decide, NewSchedule, Schedule, ScheduleId, ScheduleView};
use idlemax_core::types::{EventKind, UnixMs};
use idlemax_core::Config;

use super::http::{ApiFailure, JsonBody};
use super::AppState;
use crate::coord::schedule::{fire_due, last_job};
use crate::coord::store::Store;
use crate::state::now_ms;

/// `GET /v1/schedules`, oldest first.
pub async fn list(State(app): State<AppState>) -> Result<Json<Vec<ScheduleView>>, ApiFailure> {
    let now = now_ms();
    let views = app
        .with_store(move |store, _| {
            store
                .list_schedules()?
                .into_iter()
                .map(|s| view(store, s, now))
                .collect::<Result<Vec<_>, ApiFailure>>()
        })
        .await?;
    Ok(Json(views))
}

/// `POST /v1/schedules`: 201 with the schedule. Its first job is queued at once, so the
/// owner sees it work; later jobs follow the interval.
pub async fn add(
    State(app): State<AppState>,
    JsonBody(new): JsonBody<NewSchedule>,
) -> Result<(StatusCode, Json<ScheduleView>), ApiFailure> {
    let now = now_ms();
    let created = app
        .with_store(move |store, cfg| add_schedule(store, cfg, &new, now))
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// `DELETE /v1/schedules/{id}`: the schedule as it was. Its queued or running job is left
/// alone; the view's decision says which job that is.
pub async fn remove(
    State(app): State<AppState>,
    path: Result<Path<ScheduleId>, PathRejection>,
) -> Result<Json<ScheduleView>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let removed = app
        .with_store(move |store, _| {
            let schedule = existing(store, id)?;
            let before = view(store, schedule, now)?;
            store.remove_schedule(id)?;
            log(store, &before.schedule, "removed", now)?;
            Ok::<_, ApiFailure>(before)
        })
        .await?;
    Ok(Json(removed))
}

/// `POST /v1/schedules/{id}/pause`: no new jobs until resumed.
pub async fn pause(
    state: State<AppState>,
    path: Result<Path<ScheduleId>, PathRejection>,
) -> Result<Json<ScheduleView>, ApiFailure> {
    set_paused(state, path, true).await
}

/// `POST /v1/schedules/{id}/resume`.
pub async fn resume(
    state: State<AppState>,
    path: Result<Path<ScheduleId>, PathRejection>,
) -> Result<Json<ScheduleView>, ApiFailure> {
    set_paused(state, path, false).await
}

async fn set_paused(
    State(app): State<AppState>,
    path: Result<Path<ScheduleId>, PathRejection>,
    paused: bool,
) -> Result<Json<ScheduleView>, ApiFailure> {
    let Path(id) = path?;
    let now = now_ms();
    let updated = app
        .with_store(move |store, _| {
            existing(store, id)?;
            let schedule = store
                .set_schedule_paused(id, paused)?
                .ok_or_else(|| not_found(id))?;
            log(
                store,
                &schedule,
                if paused { "paused" } else { "resumed" },
                now,
            )?;
            view(store, schedule, now)
        })
        .await?;
    Ok(Json(updated))
}

fn add_schedule(
    store: &mut Store,
    cfg: &Config,
    new: &NewSchedule,
    now: UnixMs,
) -> Result<ScheduleView, ApiFailure> {
    admit_job(&new.spec, &cfg.quota)?;
    let schedule = store.insert_schedule(&new.spec, new.every, now)?;
    log(store, &schedule, "added", now)?;
    fire_due(store, cfg, now)?;
    let schedule = existing(store, schedule.id)?;
    view(store, schedule, now)
}

fn existing(store: &Store, id: ScheduleId) -> Result<Schedule, ApiFailure> {
    store.get_schedule(id)?.ok_or_else(|| not_found(id))
}

fn not_found(id: ScheduleId) -> ApiFailure {
    ApiFailure::NotFound(format!("schedule {id} not found"))
}

/// The schedule with its decision. Stdin can hold private prompts, so it is dropped here.
fn view(store: &Store, mut schedule: Schedule, now: UnixMs) -> Result<ScheduleView, ApiFailure> {
    let last = last_job(store, &schedule)?;
    let decision = decide(&schedule, last.as_ref(), now);
    schedule.spec.stdin = None;
    Ok(ScheduleView { schedule, decision })
}

fn log(store: &mut Store, s: &Schedule, what: &str, now: UnixMs) -> Result<(), ApiFailure> {
    let msg = format!(
        "schedule {} ({}) {what}, repeats every {}",
        s.id, s.spec.name, s.every
    );
    store.append_event(EventKind::Control, None, None, &msg, now)?;
    Ok(())
}
