//! Adapter: the owner's calls the dashboards make beside the dispatcher view: limits, the
//! mode by name, the worker stop and start, and the rule proposals.

use idlemax_core::config::{LimitReply, LimitRequest, Setting};
pub use idlemax_core::dispatch::proposal::Proposal;
use serde_json::{json, Value};

use super::{ApiClient, Result};

impl ApiClient {
    pub async fn limits(&self) -> Result<Vec<Setting>> {
        self.get("/v1/limits").await
    }

    /// `value` None puts the limit back to its default.
    pub async fn set_limit(&self, req: &LimitRequest) -> Result<LimitReply> {
        self.post("/v1/limits", Some(req)).await
    }

    /// The mode by its name, so a mode this build has no variant for still reaches the
    /// coordinator, which answers 400 when it does not know it either.
    pub async fn set_mode_named(&self, mode: &str) -> Result<Value> {
        self.post("/v1/dispatcher/mode", Some(&json!({ "mode": mode })))
            .await
    }

    pub async fn proposals(&self) -> Result<Vec<Proposal>> {
        self.get("/v1/proposals").await
    }

    /// Approves (adds the line to `rules.md`) or rejects one proposal.
    pub async fn decide_proposal(&self, id: i64, approve: bool) -> Result<Value> {
        let verb = if approve { "approve" } else { "reject" };
        self.post(&format!("/v1/proposals/{id}/{verb}"), None::<&()>)
            .await
    }
}
