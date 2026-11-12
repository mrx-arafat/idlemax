//! Adapter: which host a worker launch uses, and the launch's start on it. `workers.host`,
//! the device profile and Herdr's answer go through `pick_host`; a production device is
//! always headless (A23), and a start that fell back from Herdr says so in its first update.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use idlemax_core::config::WorkerHostChoice;
use idlemax_core::job_run::RunClock;
use idlemax_core::types::{DeviceProfile, Job, UnixMs};
use idlemax_core::worker::launch::pick_host;
use idlemax_core::worker::sandbox::{
    decide, default_hosts, Probe, SandboxFact, SandboxSetting, SandboxUse, NO_WORKER_TOKEN,
};
use idlemax_core::worker::{WorkerAction, WorkerCli, WorkerSpec};

use super::host::{HostKind, WorkerHost};
use super::sandbox::SandboxPlan;
use super::steps::{self, LaunchCtx, Progress};
use super::{new_tag, Phase, Tracked, Workers};

/// How long Herdr's answer stands, so a Herdr that hangs delays one start in this time and
/// not every one (its call may take up to 10 s on the manager's thread).
const HERDR_ANSWER_MS: i64 = 30_000;

/// The host picked for one launch, and the note that explains a fall back from Herdr.
pub(super) type Chosen = (HostKind, Arc<dyn WorkerHost>, Option<String>);

/// Finds the bwrap that may run workers, or says why none can (`sandbox::probe`); a test
/// gives one that names a stub in its own temp dir.
#[derive(Clone)]
pub(super) struct SandboxProbe(pub Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>);

impl std::fmt::Debug for SandboxProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SandboxProbe")
    }
}

/// The sandbox a launch gets, and how it runs (the Run's fact; finding 8).
pub(super) type ChosenSandbox = (Option<SandboxPlan>, Option<SandboxFact>);

/// `workers.host`, the device profile, when Herdr last answered and how, and the sandbox
/// settings (A13).
#[derive(Debug, Default)]
pub(super) struct HostPolicy {
    setting: WorkerHostChoice,
    profile: Option<DeviceProfile>,
    herdr_answer: Option<(UnixMs, bool)>,
    sandbox: SandboxSetting,
    sandbox_hosts: BTreeMap<String, Vec<String>>,
    /// `workers.claude_token_file` (review finding 6).
    claude_token: Option<PathBuf>,
    /// None: the real probe (a test build never runs bwrap).
    pub(super) probe: Option<SandboxProbe>,
}

#[cfg(not(test))]
fn default_probe() -> Result<PathBuf, String> {
    super::sandbox::probe()
}

#[cfg(test)]
fn default_probe() -> Result<PathBuf, String> {
    Err("the test build never runs bwrap".into())
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

    /// `workers.sandbox`, `workers.sandbox_hosts` and `workers.claude_token_file`, which a
    /// reload may change (A13).
    pub fn set_sandbox_policy(
        &mut self,
        setting: SandboxSetting,
        hosts: BTreeMap<String, Vec<String>>,
        claude_token: Option<PathBuf>,
    ) {
        self.policy.sandbox = setting;
        self.policy.sandbox_hosts = hosts;
        self.policy.claude_token = claude_token;
    }

    /// The sandbox of one launch (`decide`): wrapped, bare with a note, or refused. `off`
    /// probes nothing and says so.
    pub(super) fn choose_sandbox(
        &self,
        cli: WorkerCli,
        kind: HostKind,
    ) -> Result<ChosenSandbox, String> {
        // Off probes nothing, but the Run still says it ran without the sandbox (finding 10).
        if self.policy.sandbox == SandboxSetting::Off {
            return Ok((None, Some(SandboxFact::Off)));
        }
        let found = match &self.policy.probe {
            Some(probe) => (probe.0)(),
            None => default_probe(),
        };
        let probe = match &found {
            Ok(_) => Probe::Usable,
            Err(why) => Probe::Unusable(why.clone()),
        };
        match decide(self.policy.sandbox, self.policy.profile, &probe) {
            SandboxUse::Refuse(why) => Err(why),
            SandboxUse::Bare(why) => Ok((None, Some(SandboxFact::Bare { why }))),
            SandboxUse::Wrap => {
                let hosts = self
                    .policy
                    .sandbox_hosts
                    .get(cli.as_str())
                    .cloned()
                    .unwrap_or_else(|| default_hosts(cli));
                let idlemax_bin = std::env::current_exe()
                    .map_err(|e| format!("the sandbox cannot find idlemax itself: {e}"))?;
                // A Claude worker signs in with the worker token when there is one; the
                // file must be the owner's alone, or no worker starts (review finding 6).
                let claude_token = match (cli, &self.policy.claude_token) {
                    (WorkerCli::Claude, Some(file)) => {
                        super::sandbox::read_token(file)?;
                        Some(file.clone())
                    }
                    _ => None,
                };
                let note = (cli == WorkerCli::Claude && claude_token.is_none())
                    .then(|| NO_WORKER_TOKEN.to_string());
                let plan = SandboxPlan {
                    bwrap: found?,
                    idlemax_bin,
                    root: self.paths.root().to_path_buf(),
                    hosts,
                    headless: kind == HostKind::Headless,
                    claude_token,
                };
                Ok((Some(plan), Some(SandboxFact::Wrapped { note })))
            }
        }
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
        (kind, host, sandbox): (HostKind, Arc<dyn WorkerHost>, Option<SandboxPlan>),
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
            sandbox,
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
            sandbox: None,
            guarded: crate::owner_writes::snapshot(self.paths.root()),
        }
    }
}
