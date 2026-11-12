//! Adapter: the dispatcher routes of the coordinator API (projects, mode, plan).

use idlemax_core::dispatch::project::{NewProject, ProjectSettings, ProjectView};

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
