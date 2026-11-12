//! `GET /v1/dispatcher`: the dispatcher view every surface renders (D12, D17). Adapter: reads
//! the plan, the Runs, the projects and the Budgets under one store lock and calls the pure
//! `dispatcher_view`, so the picture is consistent. The quota views are read once, by the plan.

use axum::extract::State;
use axum::Json;
use idlemax_core::dispatch::label::RunLine;
use idlemax_core::dispatch::view::{dispatcher_view, DispatcherView, ViewInput, EARLIER_DAYS};
use idlemax_core::quota::owner_use::DAY_MS;
use idlemax_core::types::{JobStatus, UnixMs};
use idlemax_core::worker::run::RunState;
use idlemax_core::Config;

use super::budget::budgets;
use super::http::ApiFailure;
use super::AppState;
use crate::agent::metrics::default_device_id;
use crate::coord::dispatch::plan_with_holds;
use crate::coord::store::{Store, StoreError};
use crate::state::now_ms;

/// Runs read per state; the view never reads the whole history (the dashboard polls it).
const RUNS_PER_STATE: u32 = 200;

/// The states the view shows: the workers, then the runs waiting for review.
const SHOWN: [RunState; 8] = [
    RunState::Starting,
    RunState::Running,
    RunState::Checking,
    RunState::Stopped,
    RunState::Shed,
    RunState::Clean,
    RunState::NeedsYou,
    RunState::Rejected,
];

pub async fn view(State(app): State<AppState>) -> Result<Json<DispatcherView>, ApiFailure> {
    let now = now_ms();
    let live = std::sync::Arc::clone(app.live());
    let view = app
        .with_store(move |store, cfg| {
            Ok::<_, ApiFailure>(gather_view(store, cfg, &live.current(), now)?)
        })
        .await?;
    Ok(Json(view))
}

/// The view at `now`, from one store lock.
pub fn gather_view(
    store: &Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<DispatcherView, StoreError> {
    let gathered = plan_with_holds(store, cfg, live, now)?;
    let plan = gathered.plan;
    let paused: Vec<_> = store
        .active_jobs()?
        .into_iter()
        .filter(|j| j.status == JobStatus::Paused)
        .map(|j| j.id)
        .collect();
    let mut runs = Vec::new();
    for state in SHOWN {
        for run in store.list_runs(Some(state), None, RUNS_PER_STATE)? {
            let frozen = run.job.is_some_and(|j| paused.contains(&j));
            runs.push(RunLine { run, frozen });
        }
    }
    let budgets = budgets(store, cfg, live, &gathered.views, now)?;
    let mut days = Vec::new();
    for b in &budgets {
        let since = b.day_start_ms - DAY_MS * EARLIER_DAYS as i64;
        days.extend(store.budget_days(&b.account, since)?);
    }
    let limits: Vec<_> = store
        .devices(now, i64::MAX)?
        .into_iter()
        .filter_map(|d| Some((d.info.id, d.limits?)))
        .collect();
    let own = limits_device(live);
    let input = ViewInput {
        plan: &plan,
        runs: &runs,
        projects: &store.list_projects(None)?,
        budgets: &budgets,
        days: &days,
        review_cap: live.workers.review_cap,
        proposals_open: store.open_proposals()?,
        limits_device: Some(&own),
        limits: &limits,
        account_problems: &gathered.problems,
    };
    Ok(dispatcher_view(&input, now))
}

/// The device id of the coordinator's own machine, whose `config.toml` `POST /v1/limits`
/// edits: `agent.device_id`, else the id its agent would take from the host name.
fn limits_device(live: &Config) -> String {
    live.agent
        .device_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default_device_id(&sysinfo::System::host_name().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;

    pub(crate) const DEMO: &str = include_str!("../../../tests/fixtures/dispatcher_demo.sql");

    #[tokio::test]
    async fn the_view_needs_the_token_and_shows_the_seeded_runs_and_projects() {
        let mut store = Store::open_in_memory().unwrap();
        store.load_sql(DEMO).unwrap();
        let api = Api::start_with_store(store, Config::default()).await;
        assert_eq!(
            api.get_anon("/v1/dispatcher").await.status(),
            StatusCode::UNAUTHORIZED
        );

        let res = api.get_as("/v1/dispatcher", "localhost").await;
        assert_eq!(res.status(), StatusCode::OK);
        let v: DispatcherView = res.json().await.unwrap();
        let labels: Vec<&str> = v.workers.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["working", "checking"]);
        assert_eq!(v.workers[0].why, "first roadmap item; the check is focused");
        let review: Vec<&str> = v.review.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(review, ["ready for review", "needs you", "rejected"]);
        assert_eq!(v.needs_you.len(), 1, "{:?}", v.needs_you);
        assert_eq!(v.needs_you[0].reason, "the item says it needs the owner");
        assert_eq!(v.projects.len(), 2);
        assert_eq!(v.projects[0].waiting, 2);
        assert_eq!(v.proposals_open, 1);
    }
}
