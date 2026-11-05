//! Domain: the advisor's input snapshot: cluster state plus read-only outside signals
//! (GitHub items, agent session metadata, job templates). Plain data, no behaviour.

use serde::{Deserialize, Serialize};

use crate::protocol::ClusterStatus;
use crate::quota::QuotaView;
use crate::types::{DeviceId, Job, JobSpec, UnixMs};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdviceInput {
    pub now_ms: UnixMs,
    pub status: ClusterStatus,
    /// Recent jobs in any status (last days, newest first or any order).
    pub jobs: Vec<Job>,
    pub quota: Vec<QuotaView>,
    pub github: GithubSnapshot,
    pub sessions: Vec<SessionMeta>,
    /// Job spec templates from `Paths::jobs_dir()`: the only jobs advice may suggest.
    pub templates: Vec<JobSpec>,
}

/// One open pull request or issue.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GhItem {
    /// owner/name
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub is_draft: bool,
    pub created_ms: UnixMs,
    pub updated_ms: UnixMs,
}

impl GhItem {
    pub fn reference(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "error")]
pub enum GithubStatus {
    /// `gh` is not installed here (normal on the server).
    #[default]
    Missing,
    Ok,
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubSnapshot {
    pub status: GithubStatus,
    pub review_requested: Vec<GhItem>,
    pub assigned_prs: Vec<GhItem>,
    pub assigned_issues: Vec<GhItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSignal {
    /// The last test run seen in the session failed.
    TestsFailing,
    /// The latest todo list still has unfinished items.
    TodosOpen,
    /// The last turn was interrupted by the user.
    Interrupted,
}

impl SessionSignal {
    pub fn describe(self) -> &'static str {
        match self {
            SessionSignal::TestsFailing => "failing tests",
            SessionSignal::TodosOpen => "open todos",
            SessionSignal::Interrupted => "an interrupted turn",
        }
    }
}

/// Metadata of one agent session; never message text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    /// "claude" or "codex".
    pub tool: String,
    pub id: String,
    pub title: Option<String>,
    /// Last path component of the session's working directory.
    pub project: Option<String>,
    pub last_activity_ms: UnixMs,
    pub signals: Vec<SessionSignal>,
}

/// POST /v1/agent/advisor-inputs: what one device's agent read on its own host for the
/// advisor. Metadata only: PR and issue headers, session titles and signals, never
/// message text. Missing fields take defaults so an older or newer agent still decodes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvisorInputs {
    pub device: DeviceId,
    pub github: GithubSnapshot,
    pub sessions: Vec<SessionMeta>,
}

/// A device's newest `AdvisorInputs` and when the coordinator received them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredInputs {
    pub received_ms: UnixMs,
    pub inputs: AdvisorInputs,
}
