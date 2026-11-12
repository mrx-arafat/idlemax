//! Adapter: the runner seam of a worker, "start an agent, read its state, send it text, stop
//! it, read what ended its turn". The Herdr host implements it today and the headless host of
//! dispatcher Spec 5 later; `FakeHost` stands in for both in tests.

use std::path::PathBuf;

use idlemax_core::types::JobId;
use idlemax_core::worker::WorkerCli;
use serde::{Deserialize, Serialize};

use super::cmd::CmdError;

/// What one worker start needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The pane name (`im-<project>-<task>-<run>`).
    pub name: String,
    /// The tab label (`im: <Project>`).
    pub tab: String,
    pub folder: PathBuf,
    /// The pane's environment pairs (`pane_env`).
    pub env: Vec<(String, String)>,
    pub cli: WorkerCli,
    /// The arguments after the program name (`agent_args`, or `headless_args`).
    pub args: Vec<String>,
    /// The worker job and its attempt; the headless host names its log file and header by them.
    pub job: JobId,
    pub attempt: u32,
}

/// Which host started a worker; saved with it, so a stop, a kill and a recovery go to that host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// A record written before the headless host existed has no `host` and is a Herdr one.
    #[default]
    Herdr,
    Headless,
}

/// Where a started worker lives; saved so a stop closes only this pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handle {
    pub pane: String,
    pub workspace: String,
    pub label: String,
}

/// The agent's status as the host reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    /// Waiting for input.
    Blocked,
    Done,
    Unknown,
    /// The pane is no longer there.
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostState {
    pub status: AgentStatus,
    pub session: Option<String>,
    pub shell_pid: Option<u32>,
    /// The pids of the pane's foreground processes.
    pub foreground: Vec<u32>,
    /// The exit code of a finished worker process (headless host). None for Herdr host.
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("{call}: {source}")]
    Cmd {
        call: String,
        #[source]
        source: CmdError,
    },
    #[error("{call} failed: {why}")]
    Failed { call: String, why: String },
    #[error("{call}: no {what} in its answer")]
    Missing { call: String, what: String },
    #[error("pane {pane} is no longer the worker's (label or workspace changed); not closing it")]
    NotOurs { pane: String },
    #[error("worker {handle} was already sent a prompt")]
    AlreadySent { handle: String },
}

/// The runner seam. Every call is bounded in time by its implementation.
pub trait WorkerHost: Send + Sync {
    fn start(&self, launch: &Launch) -> Result<Handle, HostError>;
    fn state(&self, handle: &Handle) -> Result<HostState, HostError>;
    fn send(&self, handle: &Handle, text: &str) -> Result<(), HostError>;
    /// Closes the worker's pane; Ok when it is already gone.
    fn stop(&self, handle: &Handle) -> Result<(), HostError>;
    /// True while the pane is still listed with the saved label and workspace.
    fn matches(&self, handle: &Handle) -> Result<bool, HostError>;
    /// The text that ended the last turn, at most 2,000 characters (A9).
    fn end_text(&self, handle: &Handle) -> Option<String>;
    /// Whether this host is the headless runner.
    fn is_headless(&self) -> bool {
        false
    }
    /// Whether this host is available and answers.
    fn is_available(&self) -> bool {
        true
    }
}

/// A scripted host: each `state` call takes the next scripted state, the last one repeats.
#[cfg(test)]
#[derive(Default)]
pub struct FakeHost {
    pub starts: std::sync::Mutex<Vec<Launch>>,
    pub start_errors: std::sync::Mutex<std::collections::VecDeque<HostError>>,
    pub states: std::sync::Mutex<std::collections::VecDeque<HostState>>,
    pub sent: std::sync::Mutex<Vec<(Handle, String)>>,
    pub stopped: std::sync::Mutex<Vec<Handle>>,
    pub end_text: std::sync::Mutex<Option<String>>,
    /// Runs on every stop, so a test's fake processes can leave with the pane.
    #[allow(clippy::type_complexity)]
    pub on_stop: std::sync::Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Errors the next stops return, after running `on_stop`.
    pub stop_errors: std::sync::Mutex<std::collections::VecDeque<HostError>>,
    /// What `matches` answers; true when unset.
    pub owned: std::sync::Mutex<Option<bool>>,
    /// While set, `state` fails as a host that does not answer.
    pub state_error: std::sync::Mutex<bool>,
    pub headless: std::sync::Mutex<Option<bool>>,
    pub available: std::sync::Mutex<Option<bool>>,
}

#[cfg(test)]
impl FakeHost {
    pub fn push_state(&self, status: AgentStatus, session: Option<&str>, pids: &[u32]) {
        self.push_state_with_exit(status, session, pids, None);
    }

    pub fn push_state_with_exit(
        &self,
        status: AgentStatus,
        session: Option<&str>,
        pids: &[u32],
        exit_code: Option<i32>,
    ) {
        self.states.lock().unwrap().push_back(HostState {
            status,
            session: session.map(Into::into),
            shell_pid: pids.first().copied(),
            foreground: pids.iter().skip(1).copied().collect(),
            exit_code,
        });
    }
}

#[cfg(test)]
impl WorkerHost for FakeHost {
    fn is_headless(&self) -> bool {
        self.headless.lock().unwrap().unwrap_or(false)
    }

    fn is_available(&self) -> bool {
        self.available.lock().unwrap().unwrap_or(true)
    }

    fn start(&self, launch: &Launch) -> Result<Handle, HostError> {
        self.starts.lock().unwrap().push(launch.clone());
        if let Some(e) = self.start_errors.lock().unwrap().pop_front() {
            return Err(e);
        }
        let n = self.starts.lock().unwrap().len();
        Ok(Handle {
            pane: format!("p{n}"),
            workspace: "w1".into(),
            label: launch.name.clone(),
        })
    }

    fn state(&self, _handle: &Handle) -> Result<HostState, HostError> {
        if *self.state_error.lock().unwrap() {
            return Err(HostError::Failed {
                call: "herdr agent list".into(),
                why: "no server".into(),
            });
        }
        let mut states = self.states.lock().unwrap();
        let state = if states.len() > 1 {
            states.pop_front()
        } else {
            states.front().cloned()
        };
        Ok(state.unwrap_or(HostState {
            status: AgentStatus::Gone,
            session: None,
            shell_pid: None,
            foreground: Vec::new(),
            exit_code: None,
        }))
    }

    fn send(&self, handle: &Handle, text: &str) -> Result<(), HostError> {
        self.sent
            .lock()
            .unwrap()
            .push((handle.clone(), text.into()));
        Ok(())
    }

    fn stop(&self, handle: &Handle) -> Result<(), HostError> {
        self.stopped.lock().unwrap().push(handle.clone());
        if let Some(hook) = self.on_stop.lock().unwrap().as_ref() {
            hook();
        }
        match self.stop_errors.lock().unwrap().pop_front() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn matches(&self, _handle: &Handle) -> Result<bool, HostError> {
        Ok(self.owned.lock().unwrap().unwrap_or(true))
    }

    fn end_text(&self, _handle: &Handle) -> Option<String> {
        self.end_text.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_host_replays_its_states_and_records_what_it_was_told() {
        let host = FakeHost::default();
        host.push_state(AgentStatus::Idle, Some("s1"), &[10, 11]);
        host.push_state(AgentStatus::Done, Some("s1"), &[10]);
        let launch = Launch {
            name: "im-app-t-1".into(),
            tab: "im: App".into(),
            folder: "/w".into(),
            env: Vec::new(),
            cli: WorkerCli::Claude,
            args: Vec::new(),
            job: 1,
            attempt: 1,
        };
        let handle = host.start(&launch).unwrap();
        let first = host.state(&handle).unwrap();
        assert_eq!(
            (first.status, first.foreground),
            (AgentStatus::Idle, vec![11])
        );
        assert_eq!(host.state(&handle).unwrap().status, AgentStatus::Done);
        assert_eq!(host.state(&handle).unwrap().status, AgentStatus::Done);
        host.send(&handle, "go").unwrap();
        host.stop(&handle).unwrap();
        assert_eq!(host.sent.lock().unwrap()[0].1, "go");
        assert_eq!(host.stopped.lock().unwrap().len(), 1);
    }
}
