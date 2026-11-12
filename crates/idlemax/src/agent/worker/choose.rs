//! Adapter: which host a worker launch uses, and the launch's start on it. `workers.host`,
//! the device profile and Herdr's answer go through `pick_host`; a production device is
//! always headless (A23), and a start that fell back from Herdr says so in its first update.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use idlemax_core::config::WorkerHostChoice;
use idlemax_core::job_run::RunClock;
use idlemax_core::types::{DeviceProfile, Job, UnixMs};
use idlemax_core::worker::launch::pick_host;
use idlemax_core::worker::{WorkerAction, WorkerCli, WorkerSpec};

use super::host::{HostKind, WorkerHost};
use super::steps::{self, LaunchCtx, Progress};
use super::{new_tag, Phase, Tracked, Workers};

/// How long Herdr's answer stands, so a Herdr that hangs delays one start in this time and
/// not every one (its call may take up to 10 s on the manager's thread).
const HERDR_ANSWER_MS: i64 = 30_000;

/// The host picked for one launch, and the note that explains a fall back from Herdr.
pub(super) type Chosen = (HostKind, Arc<dyn WorkerHost>, Option<String>);

/// `workers.host`, the device profile, and when Herdr last answered and how.
#[derive(Debug, Default)]
pub(super) struct HostPolicy {
    setting: WorkerHostChoice,
    profile: Option<DeviceProfile>,
    herdr_answer: Option<(UnixMs, bool)>,
}

/// The session a launch passes its CLI: the saved one for a resume, a new uuid for Claude.
pub(super) fn new_session(worker: &WorkerSpec) -> Result<Option<String>, String> {
    match (worker.action, worker.cli) {
        (WorkerAction::Resume, _) => Ok(worker.session.clone()),
        (_, WorkerCli::Claude) => crate::state::new_session_id()
            .map(Some)
            .map_err(|e| e.to_string()),
        _ => Ok(None),
    }
}

impl Workers {
    /// Adds the headless host, which `workers.host` and the profile may choose.
    pub fn with_headless(mut self, host: Arc<dyn WorkerHost>) -> Self {
        self.headless = Some(host);
        self
    }

    /// `workers.host`, which a reload may change, and the device profile read at start.
    pub fn set_host_policy(&mut self, setting: WorkerHostChoice, profile: Option<DeviceProfile>) {
        self.policy.setting = setting;
        self.policy.profile = profile;
    }

    /// The host for one launch (`pick_host`), with the note that explains a fall back from
    /// Herdr; Herdr is asked only when `auto` leaves the choice to its answer.
    pub(super) fn choose_host(&mut self, now_ms: UnixMs) -> Result<Chosen, String> {
        let HostPolicy {
            setting, profile, ..
        } = self.policy;
        let asks = setting == WorkerHostChoice::Auto
            && !matches!(profile, Some(DeviceProfile::Production));
        let answers = !asks || self.herdr_answers(now_ms);
        match pick_host(setting, profile, answers) {
            WorkerHostChoice::Herdr => Ok((HostKind::Herdr, Arc::clone(&self.host), None)),
            _ => {
                let host = self
                    .headless
                    .clone()
                    .ok_or("this system has no headless worker host")?;
                let note = (asks && !answers).then(|| "headless: herdr did not answer".into());
                Ok((HostKind::Headless, host, note))
            }
        }
    }

    /// Herdr's answer, asked again once the last one is 30 s old.
    fn herdr_answers(&mut self, now_ms: UnixMs) -> bool {
        match self.policy.herdr_answer {
            Some((at, answer)) if (0..HERDR_ANSWER_MS).contains(&(now_ms - at)) => answer,
            _ => {
                let answer = self.host.is_available();
                self.policy.herdr_answer = Some((now_ms, answer));
                answer
            }
        }
    }

    /// Starts the launch's steps on their own thread and tracks the worker.
    pub(super) fn launch(
        &self,
        job: Job,
        worker: WorkerSpec,
        (kind, host): (HostKind, Arc<dyn WorkerHost>),
        session: Option<String>,
        now_ms: UnixMs,
        awake_ms: i64,
    ) -> Tracked {
        let tag = new_tag(job.id, now_ms);
        let cancel = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let rx = steps::spawn(LaunchCtx {
            host: Arc::clone(&host),
            commands: Arc::clone(&self.commands),
            procs: Arc::clone(&self.procs),
            worktrees_dir: self.paths.worktrees_dir(),
            snapshots_dir: self.paths.agent_state_dir().join("git-snapshots"),
            job: job.clone(),
            worker: worker.clone(),
            tag,
            session: session.clone(),
            cancel: Arc::clone(&cancel),
            paused: Arc::clone(&paused),
            now_ms,
            poll: self.poll,
            idle_wait: self.idle_wait,
        });
        Tracked {
            job,
            worker,
            host,
            kind,
            tag,
            clock: RunClock::start(now_ms, awake_ms),
            paused: false,
            progress: Progress {
                session,
                ..Default::default()
            },
            members: Vec::new(),
            throttled: BTreeSet::new(),
            phase: Phase::Launching {
                rx,
                cancel,
                paused,
                kill: None,
            },
            saved: None,
        }
    }
}
