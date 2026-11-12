//! One blocking batch of device readings per sample: metrics, ring buffer, kill switch
//! file, quota and interactive use. Adapter layer.

use std::path::{Path, PathBuf};

use idlemax_core::config::AgentConfig;
use idlemax_core::quota::QuotaConfig;
use idlemax_core::types::{DeviceInfo, MetricsSnapshot, QuotaSnapshot, UnixMs};
use idlemax_core::Paths;

use super::metrics::{disk, Sampler};
use super::quota_probe::{self, OwnSessions, ProbeDirs};
use super::ring::MetricsRing;

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub metrics: MetricsSnapshot,
    pub kill_switch: bool,
    pub quota: Vec<QuotaSnapshot>,
    pub interactive_use: Vec<(String, UnixMs)>,
}

pub struct Probes {
    sampler: Sampler,
    ring: MetricsRing,
    ring_failing: bool,
    pause_file: PathBuf,
    state_dir: PathBuf,
    quota: QuotaConfig,
    quota_dirs: ProbeDirs,
    agent_cfg: AgentConfig,
    own_sessions: OwnSessions,
}

impl Probes {
    pub fn new(
        sampler: Sampler,
        ring: MetricsRing,
        paths: &Paths,
        quota: QuotaConfig,
        agent_cfg: AgentConfig,
    ) -> Self {
        Self {
            sampler,
            ring,
            ring_failing: false,
            pause_file: paths.pause_file(),
            state_dir: paths.root().to_path_buf(),
            quota,
            quota_dirs: ProbeDirs::from_env(paths),
            agent_cfg,
            own_sessions: OwnSessions::new(),
        }
    }

    /// Takes the agent settings of a config reload, so the next device info reports them.
    pub fn set_agent_config(&mut self, cfg: AgentConfig) {
        self.agent_cfg = cfg;
    }

    fn has_claude_account(&self) -> bool {
        self.quota
            .accounts
            .iter()
            .any(|a| a.provider == idlemax_core::quota::Provider::Claude)
    }

    pub fn device_info(&mut self, cfg: &AgentConfig) -> DeviceInfo {
        self.sampler.device_info(cfg, self.has_claude_account())
    }

    pub fn refresh_device_info(&mut self) -> DeviceInfo {
        self.sampler
            .device_info(&self.agent_cfg, self.has_claude_account())
    }

    /// `job_dirs` are the working directories of the running jobs; the disk reading covers
    /// them too, because a job fills the filesystem it runs on.
    pub fn read(&mut self, job_dirs: &[PathBuf]) -> Reading {
        let disk_paths = disk::watched(&self.state_dir, job_dirs);
        let metrics = self.sampler.sample(&disk_paths);
        let now_ms = metrics.at_ms;
        match self.ring.append(&metrics, now_ms) {
            Ok(()) if self.ring_failing => {
                self.ring_failing = false;
                tracing::info!("metrics ring buffer writable again");
            }
            Err(e) if !self.ring_failing => {
                self.ring_failing = true;
                tracing::warn!("metrics ring buffer: {e}");
            }
            _ => {}
        }
        Reading {
            kill_switch: kill_switch_present(&self.pause_file),
            quota: quota_probe::probe(&self.quota, &self.quota_dirs, now_ms),
            interactive_use: quota_probe::interactive_use(
                &self.quota_dirs,
                &mut self.own_sessions,
                now_ms,
            ),
            metrics,
        }
    }
}

/// Fails closed: an unreadable state dir counts as the kill switch being on.
pub fn kill_switch_present(pause_file: &Path) -> bool {
    pause_file.try_exists().unwrap_or(true)
}
