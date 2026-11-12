//! Coordinator HTTP adapter: the routes of docs/API.md, bearer auth, shared state, and the
//! bridge from async handlers to the blocking store. Decisions live in `idlemax_core`.

mod advice;
mod agents;
mod budget;
pub mod caller;
#[cfg(test)]
mod dashboard_tests;
mod devices;
mod dispatcher_view;
pub mod http;
mod jobs;
mod limits;
mod mode;
pub(crate) mod plan;
mod projects;
mod proposals;
mod report;
mod runs;
mod schedules;
#[cfg(test)]
mod tests;
mod why;
#[cfg(test)]
mod worker_tests;

use std::sync::{Arc, Mutex, PoisonError};

use axum::routing::{delete, get, post};
use axum::{middleware, Extension, Router};
use idlemax_core::control::ControlError;
use idlemax_core::host::AllowedHosts;
use idlemax_core::{Config, Paths};
use tokio::task::JoinError;

use self::caller::CallerCheck;
use self::http::{ApiFailure, BearerToken, HostGuard};
use super::store::{Store, StoreError};
use super::web;
use crate::files::FileError;
use crate::live_config::LiveConfig;

/// Shared by every handler and by the coordinator's background sweeps.
#[derive(Clone)]
pub struct AppState {
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes dispatcher steps, so a sweep and a `plan --refresh` never interleave.
    dispatch_lock: Arc<tokio::sync::Mutex<()>>,
    store: Arc<Mutex<Store>>,
    live: Arc<LiveConfig>,
    config: Arc<Config>,
    paths: Arc<Paths>,
}

impl AppState {
    pub fn new(mut store: Store, config: Config, paths: Paths) -> Self {
        store.set_offline_after_ms(offline_after_ms(&config));
        Self {
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            dispatch_lock: Arc::new(tokio::sync::Mutex::new(())),
            store: Arc::new(Mutex::new(store)),
            live: Arc::new(LiveConfig::new(&paths, config.clone())),
            config: Arc::new(config),
            paths: Arc::new(paths),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The dispatcher settings as the file says now (A2). Blocking: call it inside a
    /// `with_store` closure.
    pub fn live(&self) -> &Arc<LiveConfig> {
        &self.live
    }

    pub fn dispatch_lock(&self) -> &Arc<tokio::sync::Mutex<()>> {
        &self.dispatch_lock
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Runs `f` on the blocking pool with the store locked; one call is one consistent step.
    pub async fn with_store<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut Store, &Config) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<JoinError> + Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let config = Arc::clone(&self.config);
        tokio::task::spawn_blocking(move || {
            // A panic mid-call cannot leave SQLite half-written (transactions roll back on
            // drop), so a poisoned lock is safe to reuse.
            let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut store, &config)
        })
        .await?
    }
}

impl From<JoinError> for ApiFailure {
    fn from(e: JoinError) -> Self {
        ApiFailure::Internal(format!("store task: {e}"))
    }
}

pub fn router(state: AppState, token: &str) -> Router {
    let check = CallerCheck::for_home(state.paths().root());
    router_with(state, token, check)
}

/// The router with the given caller check; tests pass one with a fixed lookup.
pub fn router_with(state: AppState, token: &str, check: CallerCheck) -> Router {
    let v1 = Router::new()
        .route("/v1/agent/heartbeat", post(agents::heartbeat))
        .route("/v1/agent/job-update", post(agents::job_update))
        .route("/v1/agent/advisor-inputs", post(agents::advisor_inputs))
        .route("/v1/agent/dispatch-inputs", post(plan::dispatch_inputs))
        .route("/v1/status", get(jobs::status))
        .route("/v1/jobs", get(jobs::list).post(jobs::add))
        .route("/v1/jobs/{id}", get(jobs::get_one))
        .route("/v1/jobs/{id}/cancel", post(jobs::cancel))
        .route("/v1/jobs/{id}/why", get(why::why))
        .route("/v1/why", get(why::why_all))
        .route("/v1/quota", get(jobs::quota))
        .route("/v1/report", get(report::report))
        .route("/v1/control", post(jobs::control))
        .route("/v1/events", get(jobs::events))
        .route("/v1/advice", get(advice::list))
        .route("/v1/advice/refresh", post(advice::refresh))
        .route("/v1/advice/{id}/accept", post(advice::accept))
        .route("/v1/advice/{id}/dismiss", post(advice::dismiss))
        .route("/v1/schedules", get(schedules::list).post(schedules::add))
        .route("/v1/schedules/{id}", delete(schedules::remove))
        .route("/v1/schedules/{id}/pause", post(schedules::pause))
        .route("/v1/schedules/{id}/resume", post(schedules::resume))
        .route("/v1/dispatcher/mode", post(mode::set))
        .route("/v1/dispatcher", get(dispatcher_view::view))
        .route("/v1/proposals", get(proposals::list))
        .route("/v1/proposals/{id}/approve", post(proposals::approve))
        .route("/v1/proposals/{id}/reject", post(proposals::reject))
        .route("/v1/plan", get(plan::plan))
        .route("/v1/plan/refresh", post(plan::refresh))
        .route("/v1/limits", get(limits::list).post(limits::set))
        .route("/v1/devices/{id}/on", post(devices::on))
        .route("/v1/devices/{id}/off", post(devices::off))
        .route("/v1/projects", get(projects::list).post(projects::add))
        .route("/v1/projects/{id}/on", post(projects::on))
        .route("/v1/projects/{id}/off", post(projects::off))
        .route("/v1/projects/{id}/settings", post(projects::settings))
        .route("/v1/runs", get(runs::list))
        .route("/v1/runs/{id}", get(runs::get_one))
        .route("/v1/runs/{id}/keep", post(runs::keep))
        .route("/v1/runs/{id}/discard", post(runs::discard))
        .route("/v1/workers/stop", post(runs::stop))
        .route("/v1/workers/start", post(runs::start))
        .route_layer(middleware::from_fn_with_state(
            BearerToken::new(token),
            http::require_bearer,
        ));
    let guard = HostGuard::new(coordinator_hosts(state.config()), HOST_HINT);
    Router::new()
        .route("/healthz", get(http::healthz))
        .route("/", get(web::index))
        .route("/dashboard.js", get(web::script))
        .merge(v1)
        .fallback(http::no_route)
        .method_not_allowed_fallback(http::wrong_method)
        .with_state(state)
        .layer(Extension(check))
        .layer(middleware::from_fn_with_state(guard, http::require_host))
}

const HOST_HINT: &str = "add the name to coordinator.allowed_hosts in the coordinator's config.toml, or reach it by its IP address";

/// Names the coordinator answers to besides IP addresses and `localhost`: the host of its own
/// `coordinator.url` and the ones the owner lists.
fn coordinator_hosts(cfg: &Config) -> AllowedHosts {
    let own = reqwest::Url::parse(&cfg.coordinator.url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    AllowedHosts::new(cfg.coordinator.allowed_hosts.iter().cloned().chain(own))
}

impl From<StoreError> for ApiFailure {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::JobNotFound(_) | StoreError::ScheduleNotFound(_) => {
                ApiFailure::NotFound(e.to_string())
            }
            StoreError::AccountLeased(_)
            | StoreError::ScheduleExists { .. }
            | StoreError::LimitReached { .. } => ApiFailure::Conflict(e.to_string()),
            StoreError::Invalid(_) | StoreError::InvalidSpec(_) => {
                ApiFailure::BadRequest(e.to_string())
            }
            other => ApiFailure::Internal(other.to_string()),
        }
    }
}

impl From<FileError> for ApiFailure {
    fn from(e: FileError) -> Self {
        ApiFailure::Internal(format!("{e}: {}", e.source))
    }
}

impl From<ControlError> for ApiFailure {
    fn from(e: ControlError) -> Self {
        match e {
            ControlError::UnknownJob(_) | ControlError::UnknownDevice(_) => {
                ApiFailure::NotFound(e.to_string())
            }
            ControlError::JobFinished(_) => ApiFailure::Conflict(e.to_string()),
            ControlError::CancelNeedsJob
            | ControlError::NotLocal
            | ControlError::InvalidJob(_)
            | ControlError::InvalidDevice(_)
            | ControlError::UnknownAccount(_)
            | ControlError::DispatcherOnly(_) => ApiFailure::BadRequest(e.to_string()),
        }
    }
}

pub use self::advice::run_refresh;
#[cfg(test)]
pub(crate) use self::budget::tests as budget_test_world;
pub use self::budget::{budgets, budgets_with_table, close_quota_days, record_open_days, Gates};
pub(crate) use self::dispatcher_view::gather_view;
pub(crate) use self::jobs::quota_views;

/// Query `limit`: default 100, at most 1000.
fn page_limit(requested: Option<u32>) -> u32 {
    requested.unwrap_or(100).clamp(1, 1000)
}

/// A device is offline after this long without a heartbeat.
pub fn offline_after_ms(cfg: &Config) -> i64 {
    i64::try_from(cfg.coordinator.offline_after_secs.saturating_mul(1000)).unwrap_or(i64::MAX)
}
