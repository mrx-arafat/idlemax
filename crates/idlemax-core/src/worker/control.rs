//! Domain: stopping and continuing workers as a whole. The owner's global stop moves every
//! live Run to `stopped`, which only the owner's start moves on (D11); an account stop (the
//! 5-hour line, the cap, a rate limit) and a device switched off move that scope's live Runs
//! to `shed`, so they continue by themselves when it allows them again (A18, A23). Pure.

use serde::{Deserialize, Serialize};

use super::run::{Run, RunState};
use super::RunId;
use crate::types::{ControlScope, UnixMs};

/// `POST /v1/workers/stop`: `global` is the owner's stop, `account` the stop Spec 3 asks
/// for. A device is stopped by switching it off (`POST /v1/devices/<id>/off`), which this
/// route points to.
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
    #[error("workers are stopped as a whole, per account or per device; not per {0}")]
    Scope(&'static str),
}

/// The live Runs a stop moves, each with the state it had.
pub fn plan_stop(
    runs: &[Run],
    scope: &ControlScope,
    now_ms: UnixMs,
) -> Result<Vec<(Run, RunState)>, StopError> {
    let (target, account, device) = match scope {
        ControlScope::Global => (RunState::Stopped, None, None),
        ControlScope::Account(a) => (RunState::Shed, Some(a.as_str()), None),
        ControlScope::Device(d) => (RunState::Shed, None, Some(d.as_str())),
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
        .filter(|r| device.is_none_or(|d| r.device == d))
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
        // A device switched off sheds its own live Runs, so they continue once it is on.
        let mut on_box = runs.clone();
        on_box[1].device = "box".into();
        let off = plan_stop(&on_box, &ControlScope::Device("box".into()), 9).unwrap();
        assert_eq!(off.len(), 1);
        assert_eq!((off[0].0.id, off[0].0.state), (2, RunState::Shed));
        assert!(plan_stop(&runs, &ControlScope::Job(1), 9).is_err());
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
