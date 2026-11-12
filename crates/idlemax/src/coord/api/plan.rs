//! Dispatcher routes: the agents' dispatch feed. Adapter: stores the feed, turns it into
//! candidate projects and owner activity, and replies with the device's switched-on projects.

use axum::extract::State;
use axum::Json;
use idlemax_core::control::may_assign;
use idlemax_core::dispatch::inputs::{DispatchInputs, DispatchReply};
use idlemax_core::dispatch::Mode;
use idlemax_core::types::UnixMs;
use idlemax_core::Config;

use super::http::{ApiFailure, JsonBody};
use super::offline_after_ms;
use super::AppState;
use crate::coord::store::Store;
use crate::state::now_ms;

/// `POST /v1/agent/dispatch-inputs`.
pub async fn dispatch_inputs(
    State(app): State<AppState>,
    JsonBody(inputs): JsonBody<DispatchInputs>,
) -> Result<Json<DispatchReply>, ApiFailure> {
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
/// while the mode is `off` or the device may not take work, so one gate stops every project
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
    let local_paused = store
        .devices(now, offline_after_ms(cfg))?
        .into_iter()
        .find(|d| d.info.id == inputs.device)
        .is_some_and(|d| d.local_paused);
    if !may_assign(&store.paused_scopes()?, &inputs.device, local_paused) {
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
mod tests {
    use idlemax_core::dispatch::inputs::RepoReport;
    use idlemax_core::dispatch::project::ProjectView;
    use reqwest::StatusCode;

    use super::super::projects::project_views;
    use super::super::tests::Api;
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn feed() -> DispatchInputs {
        let repo = |name: &str, working| RepoReport {
            path: format!("/code/{name}"),
            name: name.into(),
            default_branch: Some("main".into()),
            working,
            ..RepoReport::default()
        };
        DispatchInputs {
            device: "mac".into(),
            herdr: true,
            repos: vec![repo("app", true), repo("lib", false)],
        }
    }

    #[tokio::test]
    async fn a_feed_adds_each_repository_once_off_and_marks_owner_activity() {
        let api = Api::start().await;
        for _ in 0..2 {
            let res = api.post("/v1/agent/dispatch-inputs", &feed()).await;
            assert_eq!(res.status(), StatusCode::OK);
        }
        let all: Vec<ProjectView> = api
            .get_as("/v1/projects", "localhost")
            .await
            .json()
            .await
            .unwrap();
        let shown: Vec<(&str, bool, bool)> = all
            .iter()
            .map(|v| (v.project.name.as_str(), v.project.enabled, v.owner_active))
            .collect();
        assert_eq!(shown, [("app", false, true), ("lib", false, false)]);
        assert_eq!(all[0].project.settings.base_branch.as_deref(), Some("main"));
    }

    #[test]
    fn owner_activity_fades_and_mode_off_replies_with_no_project() {
        let mut store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        ingest_feed(&mut store, &cfg, &cfg, &feed(), T0).unwrap();
        let app = store.list_projects(None).unwrap()[0].project.id;
        store.set_project_enabled(app, true).unwrap();
        let active = |store: &Store, at| project_views(store, None, at).unwrap()[0].owner_active;
        assert!(active(&store, T0 + 60_000));
        assert!(!active(&store, T0 + 31 * 60_000));

        let reply = ingest_feed(&mut store, &cfg, &cfg, &feed(), T0).unwrap();
        assert_eq!(reply.projects.len(), 1);
        let mut off = Config::default();
        off.dispatcher.mode = Mode::Off;
        let reply = ingest_feed(&mut store, &cfg, &off, &feed(), T0).unwrap();
        assert!(reply.projects.is_empty());
    }

    #[test]
    fn a_step_queues_one_check_job_in_the_base_checkout() {
        use idlemax_core::dispatch::inputs::Checkout;
        use idlemax_core::dispatch::project::ProjectSettings;
        let mut store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let mut inputs = feed();
        inputs.repos[1].base_commit = Some("c1".into());
        inputs.repos[1].checkout = Some(Checkout {
            path: "/state/checks/2".into(),
            commit: Some("c1".into()),
            error: None,
        });
        ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();
        let lib = store.list_projects(None).unwrap()[1].project.clone();
        store.set_project_enabled(lib.id, true).unwrap();
        let settings = ProjectSettings {
            base_branch: Some("main".into()),
            check_command: vec!["cargo".into(), "test".into()],
            ..Default::default()
        };
        store.set_project_settings(lib.id, &settings).unwrap();
        ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();

        let mut off = Config::default();
        off.dispatcher.mode = Mode::Off;
        crate::dispatch::step(&mut store, &off, T0).unwrap();
        assert!(store.queued_jobs().unwrap().is_empty());

        crate::dispatch::step(&mut store, &cfg, T0).unwrap();
        crate::dispatch::step(&mut store, &cfg, T0 + 60_000).unwrap();
        let queued = store.queued_jobs().unwrap();
        assert_eq!(queued.len(), 1, "{queued:?}");
        assert_eq!(queued[0].spec.name, "dispatch-check:lib");
        assert_eq!(queued[0].spec.cwd.as_deref(), Some("/state/checks/2"));
    }
}
