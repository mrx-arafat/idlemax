//! Domain: the Run, the record of one worker on one task from its first launch to the
//! owner's review, with its ten states and the moves allowed between them. Pure.

use serde::{Deserialize, Serialize};

use super::report::CheckFacts;
use super::RunId;
use crate::quota::Provider;
use crate::types::{DeviceId, JobId, QuotaSnapshot, UnixMs};

/// Where a Run is. The ten shared values of docs/DISPATCHER.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Starting,
    Running,
    Checking,
    Clean,
    NeedsYou,
    Rejected,
    /// The owner stopped it; only the owner's start moves it on (D11).
    Stopped,
    /// Back in the queue; it restarts by itself when there is room (D14).
    Shed,
    Kept,
    Discarded,
}

impl RunState {
    pub const ALL: [RunState; 10] = [
        RunState::Starting,
        RunState::Running,
        RunState::Checking,
        RunState::Clean,
        RunState::NeedsYou,
        RunState::Rejected,
        RunState::Stopped,
        RunState::Shed,
        RunState::Kept,
        RunState::Discarded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Starting => "starting",
            RunState::Running => "running",
            RunState::Checking => "checking",
            RunState::Clean => "clean",
            RunState::NeedsYou => "needs_you",
            RunState::Rejected => "rejected",
            RunState::Stopped => "stopped",
            RunState::Shed => "shed",
            RunState::Kept => "kept",
            RunState::Discarded => "discarded",
        }
    }

    /// The moves of D11, D14 and D16; every other move is refused.
    pub fn may_become(self, next: RunState) -> bool {
        use RunState::*;
        match self {
            Starting => matches!(next, Running | Stopped | Shed | NeedsYou),
            Running => matches!(next, Checking | Stopped | Shed | NeedsYou),
            Checking => matches!(next, Clean | NeedsYou | Rejected | Stopped | Shed),
            Stopped => matches!(next, Shed | Discarded),
            Shed => matches!(next, Starting | Discarded),
            Clean | NeedsYou | Rejected => matches!(next, Kept | Discarded),
            Kept | Discarded => false,
        }
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunState::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| format!("unknown run state: {s}"))
    }
}

/// The result of idlemax's own check of a finished task (D8). Kept beside the state, so it
/// stays visible after a keep or a discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Clean,
    NeedsYou,
    Rejected,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Clean => "clean",
            Verdict::NeedsYou => "needs_you",
            Verdict::Rejected => "rejected",
        }
    }
}

/// What the device found when a launch ended.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckFacts>,
    /// The text that ended a turn in which no commit was added (A9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_text: Option<String>,
}

/// One worker on one task (the Run of docs/DISPATCHER.md, D10 item 1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: RunId,
    pub device: DeviceId,
    pub project: String,
    /// The Task id.
    pub task: String,
    pub task_title: String,
    /// The Task's source, the "task type" of D10 item 1 (A24).
    pub source: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub session: Option<String>,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub concern: Option<String>,
    /// Why the task was picked, copied from the Plan slot at launch (A5).
    pub reason: String,
    /// One line on what was done.
    pub summary: Option<String>,
    pub facts: RunFacts,
    /// The live or last job of this Run.
    pub job: Option<JobId>,
    /// Jobs started for this Run.
    pub launches: u32,
    /// Launches in a row that never reached `running`.
    pub start_tries: u32,
    pub start_ms: UnixMs,
    pub end_ms: Option<UnixMs>,
    pub updated_ms: UnixMs,
    pub quota_before: Vec<QuotaSnapshot>,
    pub quota_after: Vec<QuotaSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn may_become_allows_exactly_the_listed_moves() {
        use RunState::*;
        let allowed = [
            (Starting, Running),
            (Starting, Stopped),
            (Starting, Shed),
            (Starting, NeedsYou),
            (Running, Checking),
            (Running, Stopped),
            (Running, Shed),
            (Running, NeedsYou),
            (Checking, Clean),
            (Checking, NeedsYou),
            (Checking, Rejected),
            (Checking, Stopped),
            (Checking, Shed),
            (Stopped, Shed),
            (Stopped, Discarded),
            (Shed, Starting),
            (Shed, Discarded),
            (Clean, Kept),
            (Clean, Discarded),
            (NeedsYou, Kept),
            (NeedsYou, Discarded),
            (Rejected, Kept),
            (Rejected, Discarded),
        ];
        for from in RunState::ALL {
            for to in RunState::ALL {
                assert_eq!(
                    from.may_become(to),
                    allowed.contains(&(from, to)),
                    "{from} -> {to}"
                );
            }
        }
    }

    #[test]
    fn states_round_trip_as_snake_case() {
        for st in RunState::ALL {
            let json = serde_json::to_value(st).unwrap();
            assert_eq!(json, st.as_str());
            assert_eq!(st.as_str().parse::<RunState>().unwrap(), st);
        }
    }
}
