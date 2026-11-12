//! Domain: what a device reports about a worker launch in `JobUpdate.worker`: its phase,
//! the session, branch and worktree, and on the last update the facts of its check. Pure.

use serde::{Deserialize, Serialize};

use super::{RunId, WorkerCli};
use crate::types::QuotaSnapshot;

/// Longest `end_text` a device sends, in characters (A9).
pub const MAX_END_TEXT_CHARS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerPhase {
    #[default]
    Preparing,
    Running,
    Checking,
    Done,
}

/// The worker part of a job update. The last update of a launch carries the session, branch
/// and worktree again, so a lost phase update loses nothing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerReport {
    pub run: RunId,
    pub phase: WorkerPhase,
    pub session: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<CheckFacts>,
    /// The text that ended a turn in which no commit was added, at most
    /// `MAX_END_TEXT_CHARS`; the only worker output that leaves the device (A9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_text: Option<String>,
}

/// Lines added and removed in one changed file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileDiff {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

/// The diff of a worker's branch against its base commit.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiffStat {
    pub files: Vec<FileDiff>,
}

impl DiffStat {
    /// Added plus removed lines over every file.
    pub fn lines(&self) -> u64 {
        self.files.iter().map(|f| f.added + f.removed).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAnswer {
    Approve,
    Concern,
}

/// The independent review of a high-risk diff (D8), with the readings of the account that
/// paid for it (D10 item 1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewFacts {
    pub cli: WorkerCli,
    pub account: String,
    pub model: String,
    /// None when the review did not happen or gave no readable answer.
    pub answer: Option<ReviewAnswer>,
    pub concern: Option<String>,
    pub quota_before: Vec<QuotaSnapshot>,
    pub quota_after: Vec<QuotaSnapshot>,
}

/// What the device found in the worktree after the turn (D8).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckFacts {
    /// Commits since the base commit.
    pub commits: u32,
    /// Tracked paths left modified and uncommitted.
    pub modified: Vec<String>,
    /// New untracked paths.
    pub untracked: Vec<String>,
    pub diff: DiffStat,
    /// Subject of the last commit.
    pub summary: Option<String>,
    /// Exit code of the project's check on the base commit; None when not recorded.
    pub baseline_before: Option<i32>,
    /// Exit code of the same check after the turn; None when not recorded.
    pub baseline_after: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewFacts>,
}
