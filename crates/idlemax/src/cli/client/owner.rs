//! Adapter: the owner's calls the dashboards make beside the dispatcher view: limits, the
//! mode by name, the worker stop and start, and the rule proposals.

use idlemax_core::config::{LimitReply, LimitRequest, Setting};
use idlemax_core::types::UnixMs;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{ApiClient, Result};

/// One rule proposal as `GET /v1/proposals` lists it.
// SHORTCUT: mirrors the wire shape Spec 4 Task 6 fixes, ceiling: drifts if Task 6 changes
// it, upgrade trigger: Task 6 lands its core type, then use that one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Proposal {
    pub id: i64,
    pub at_ms: UnixMs,
    pub run: i64,
    pub line: String,
    pub reason: String,
    pub status: String,
}

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
