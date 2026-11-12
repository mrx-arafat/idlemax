//! Run routes (adapter): read the Runs of the dispatcher's workers. The `launch` column, which
//! holds the brief, is never read here, so it never leaves the store.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::Json;
use idlemax_core::worker::run::{Run, RunState};
use idlemax_core::worker::RunId;
use serde::Deserialize;

use super::http::ApiFailure;
use super::{page_limit, AppState};

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

#[cfg(test)]
mod tests {
    use idlemax_core::worker::run::RunLaunch;
    use idlemax_core::Config;
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;
    use crate::coord::store::Store;

    fn run(project: &str, state: RunState) -> Run {
        Run {
            device: "mac".into(),
            project: project.into(),
            task: "roadmap:1".into(),
            account: "max".into(),
            reason: "ready".into(),
            state,
            start_ms: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn runs_are_filtered_by_state_and_project_and_an_unknown_id_is_404() {
        let mut store = Store::open_in_memory().unwrap();
        let launch = RunLaunch {
            brief: "secret brief".into(),
            ..Default::default()
        };
        for (project, state) in [
            ("a", RunState::Starting),
            ("b", RunState::Starting),
            ("a", RunState::Clean),
        ] {
            store.insert_run(&run(project, state), &launch).unwrap();
        }
        let api = Api::start_with_store(store, Config::default()).await;
        let get = |path: &'static str| api.get_as(path, "localhost");

        let res = get("/v1/runs?state=starting&project=a").await;
        let text = res.text().await.unwrap();
        assert!(!text.contains("secret brief"), "{text}");
        let runs: Vec<Run> = serde_json::from_str(&text).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].id, runs[0].project.as_str()), (1, "a"));
        let all: Vec<Run> = get("/v1/runs").await.json().await.unwrap();
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 2, 1]);
        assert_eq!(
            get("/v1/runs?state=bogus").await.status(),
            StatusCode::BAD_REQUEST
        );

        let one: Run = get("/v1/runs/2").await.json().await.unwrap();
        assert_eq!(one.project, "b");
        let res = get("/v1/runs/999").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["error"], "run 999 not found");
    }
}
