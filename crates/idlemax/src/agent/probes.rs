//! One blocking batch of device readings per sample: metrics, ring buffer, kill switch
//! file, quota and interactive use. Adapter layer.

use std::path::{Path, PathBuf};

use idlemax_core::config::AgentConfig;
use idlemax_core::quota::QuotaConfig;
use idlemax_core::types::{DeviceInfo, MetricsSnapshot, QuotaSnapshot, UnixMs};
use idlemax_core::Paths;

use super::metrics::Sampler;
use super::quota_probe::{self, ProbeDirs};
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
    quota: QuotaConfig,
    quota_dirs: ProbeDirs,
}

impl Probes {
    pub fn new(sampler: Sampler, ring: MetricsRing, paths: &Paths, quota: QuotaConfig) -> Self {
        Self {
            sampler,
            ring,
            ring_failing: false,
            pause_file: paths.pause_file(),
            quota,
            quota_dirs: ProbeDirs::from_env(paths),
        }
    }

    pub fn device_info(&mut self, cfg: &AgentConfig) -> DeviceInfo {
        self.sampler.device_info(cfg)
    }

    pub fn read(&mut self) -> Reading {
        let metrics = self.sampler.sample();
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
            interactive_use: quota_probe::interactive_use(&self.quota_dirs, now_ms),
            metrics,
        }
    }
}

/// Fails closed: an unreadable state dir counts as the kill switch being on.
pub fn kill_switch_present(pause_file: &Path) -> bool {
    pause_file.try_exists().unwrap_or(true)
}
