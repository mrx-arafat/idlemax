//! Domain: stopping and continuing workers as a whole. The owner's global stop moves every
//! live Run to `stopped`, which only the owner's start moves on (D11); an account stop (the
//! 5-hour line, the cap, a rate limit) moves that account's live Runs to `shed`, so they
//! continue by themselves when the account allows it (A18). Pure.

use serde::{Deserialize, Serialize};

use super::run::{Run, RunState};
use super::RunId;
use crate::types::{ControlScope, UnixMs};

/// `POST /v1/workers/stop`: `global` is the owner's stop, `account` the stop Spec 3 asks
/// for; `device` and `job` are refused until Spec 5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopRequest {
    pub scope: ControlScope,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StopReply {
    /// The Runs this stop moved.
    pub workers: Vec<RunId>,
    /// The owner's stop is stored: nothing launches until `idlemax start`.
    pub stop_in_force: bool,
}

/// `POST /v1/workers/start`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StartReply {
    /// The stopped Runs put back in the queue.
    pub workers: Vec<RunId>,
    /// Pauses still in force; nothing is launched into a paused scope.
    pub paused: Vec<ControlScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StopError {
    #[error("workers are stopped as a whole or per account; a {0} scope is not supported yet")]
    Scope(&'static str),
}

/// The live Runs a stop moves, each with the state it had.
pub fn plan_stop(
    runs: &[Run],
    scope: &ControlScope,
    now_ms: UnixMs,
) -> Result<Vec<(Run, RunState)>, StopError> {
    let (target, account) = match scope {
        ControlScope::Global => (RunState::Stopped, None),
        ControlScope::Account(a) => (RunState::Shed, Some(a.as_str())),
        ControlScope::Device(_) => return Err(StopError::Scope("device")),
        ControlScope::Job(_) => return Err(StopError::Scope("job")),
    };
    let live = |r: &&Run| {
        matches!(
            r.state,
            RunState::Starting | RunState::Running | RunState::Checking
        )
    };
    Ok(runs
        .iter()
        .filter(live)
        .filter(|r| account.is_none_or(|a| r.account == a))
        .map(|r| (moved(r, target, now_ms), r.state))
        .collect())
}

/// The owner's start: every `stopped` Run goes back in the queue, which the launch step
/// drains one at a time after it checked the line state and the account again (D11).
pub fn plan_start(runs: &[Run], now_ms: UnixMs) -> Vec<(Run, RunState)> {
    runs.iter()
        .filter(|r| r.state == RunState::Stopped)
        .map(|r| (moved(r, RunState::Shed, now_ms), r.state))
        .collect()
}

fn moved(run: &Run, state: RunState, now_ms: UnixMs) -> Run {
    Run {
        state,
        updated_ms: now_ms,
        ..run.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: i64, account: &str, state: RunState) -> Run {
        Run {
            id,
            account: account.into(),
            state,
            ..Default::default()
        }
    }

    #[test]
    fn a_global_stop_stops_live_runs_and_an_account_stop_sheds_its_own() {
        let runs = [
            run(1, "max", RunState::Running),
            run(2, "work", RunState::Starting),
            run(3, "max", RunState::Clean),
        ];
        let all = plan_stop(&runs, &ControlScope::Global, 9).unwrap();
        let states: Vec<_> = all
            .iter()
            .map(|(r, before)| (r.id, r.state, *before))
            .collect();
        assert_eq!(
            states,
            [
                (1, RunState::Stopped, RunState::Running),
                (2, RunState::Stopped, RunState::Starting)
            ]
        );
        let max = plan_stop(&runs, &ControlScope::Account("max".into()), 9).unwrap();
        assert_eq!(max.len(), 1);
        assert_eq!((max[0].0.id, max[0].0.state), (1, RunState::Shed));
        assert!(plan_stop(&runs, &ControlScope::Device("mac".into()), 9).is_err());
    }

    #[test]
    fn a_start_queues_every_stopped_run() {
        let runs = [
            run(1, "max", RunState::Stopped),
            run(2, "max", RunState::Shed),
        ];
        let started = plan_start(&runs, 9);
        assert_eq!(started.len(), 1);
        assert_eq!((started[0].0.id, started[0].0.state), (1, RunState::Shed));
    }
}
