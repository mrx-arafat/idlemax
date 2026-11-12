//! Adapter: the dispatcher routes of the coordinator API (projects, mode, plan, view).

use idlemax_core::dispatch::plan::Plan;
use idlemax_core::dispatch::project::{NewProject, ProjectSettings, ProjectView};
use idlemax_core::dispatch::view::DispatcherView;
use idlemax_core::dispatch::{Mode, ModeReply, ModeRequest};
use idlemax_core::types::Job;
use idlemax_core::worker::control::{StartReply, StopReply, StopRequest};
use idlemax_core::worker::run::Run;
use idlemax_core::worker::RunId;

use super::{ApiClient, Result};

impl ApiClient {
    pub async fn projects(&self) -> Result<Vec<ProjectView>> {
        self.get("/v1/projects").await
    }

    /// The project and whether it is new (201) rather than known (200).
    pub async fn add_project(&self, new: &NewProject) -> Result<(ProjectView, bool)> {
        let path = "/v1/projects";
        let req = self.http.post(self.url(path)).json(new);
        let (status, view) = self.send_with_status(req, path).await?;
        Ok((view, status == 201))
    }

    pub async fn switch_project(&self, id: i64, on: bool) -> Result<ProjectView> {
        let word = if on { "on" } else { "off" };
        self.post(&format!("/v1/projects/{id}/{word}"), None::<&()>)
            .await
    }

    pub async fn set_project_settings(
        &self,
        id: i64,
        settings: &ProjectSettings,
    ) -> Result<ProjectView> {
        self.post(&format!("/v1/projects/{id}/settings"), Some(settings))
            .await
    }
}

impl ApiClient {
    pub async fn set_mode(&self, mode: Mode) -> Result<ModeReply> {
        self.post("/v1/dispatcher/mode", Some(&ModeRequest { mode }))
            .await
    }
}

impl ApiClient {
    pub async fn stop_workers(&self, req: &StopRequest) -> Result<StopReply> {
        self.post("/v1/workers/stop", Some(req)).await
    }

    pub async fn start_workers(&self) -> Result<StartReply> {
        self.post("/v1/workers/start", None::<&()>).await
    }

    pub async fn runs(&self) -> Result<Vec<Run>> {
        self.get("/v1/runs?limit=1000").await
    }

    pub async fn run(&self, id: RunId) -> Result<Run> {
        self.get(&format!("/v1/runs/{id}")).await
    }

    pub async fn keep_run(&self, id: RunId, merge: bool) -> Result<Job> {
        let body = serde_json::json!({ "merge": merge });
        self.post(&format!("/v1/runs/{id}/keep"), Some(&body)).await
    }

    pub async fn discard_run(&self, id: RunId) -> Result<Job> {
        self.post(&format!("/v1/runs/{id}/discard"), None::<&()>)
            .await
    }
}

impl ApiClient {
    pub async fn plan(&self) -> Result<Plan> {
        self.get("/v1/plan").await
    }

    /// Runs the dispatcher step with a pass asked for, then returns the plan.
    pub async fn refresh_plan(&self) -> Result<Plan> {
        self.post("/v1/plan/refresh", None::<&()>).await
    }
}

impl ApiClient {
    /// `GET /v1/dispatcher`; a coordinator that predates it answers 404.
    pub async fn dispatcher(&self) -> Result<DispatcherView> {
        self.get("/v1/dispatcher").await
    }
}
