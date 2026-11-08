//! Adapter: the schedule routes of the coordinator API.

use idlemax_core::schedule::{NewSchedule, ScheduleId, ScheduleView};

use super::{ApiClient, Result};

impl ApiClient {
    pub async fn schedules(&self) -> Result<Vec<ScheduleView>> {
        self.get("/v1/schedules").await
    }

    pub async fn add_schedule(&self, new: &NewSchedule) -> Result<ScheduleView> {
        self.post("/v1/schedules", Some(new)).await
    }

    pub async fn remove_schedule(&self, id: ScheduleId) -> Result<ScheduleView> {
        let path = format!("/v1/schedules/{id}");
        self.send(self.http.delete(self.url(&path)), &path).await
    }

    pub async fn pause_schedule(&self, id: ScheduleId) -> Result<ScheduleView> {
        self.post(&format!("/v1/schedules/{id}/pause"), None::<&()>)
            .await
    }

    pub async fn resume_schedule(&self, id: ScheduleId) -> Result<ScheduleView> {
        self.post(&format!("/v1/schedules/{id}/resume"), None::<&()>)
            .await
    }
}
