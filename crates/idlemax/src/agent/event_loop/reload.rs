//! Config reload for the agent event loop (adapter): re-reads `config.toml` and applies what a
//! running agent can take at once. The domain `plan_reload` decides which changes those are.

use idlemax_core::config::{plan_reload, Config, ReloadReport};
use idlemax_core::types::Os;
use tracing::{info, warn};

use super::bounded::blocking_within;
use super::sample::PROBE_LIMIT;
use super::Agent;
use crate::agent::api::ReloadRequest;
use crate::state;

impl Agent {
    pub(super) async fn on_reload(&mut self, request: ReloadRequest) {
        let answer = self.reload_config().await;
        if let Err(why) = &answer {
            warn!("config reload refused, the agent keeps its settings: {why}");
        }
        // The caller may have given up waiting; nothing to do then.
        let _ = request.reply.send(answer);
    }

    async fn reload_config(&mut self) -> Result<ReloadReport, String> {
        let paths = self.paths.clone();
        let new = tokio::task::spawn_blocking(move || state::load_config(&paths))
            .await
            .map_err(|e| format!("reading config.toml: {e}"))?
            .map_err(|e| e.to_string())?;
        let report = plan_reload(&self.config, &new).map_err(|e| e.to_string())?;
        if report.is_empty() {
            return Ok(report);
        }
        if report.guard_changed() {
            self.apply_guard(&new)?;
        }
        let allowlist = report.applies("agent.command_allowlist");
        if allowlist {
            let list = new.agent.command_allowlist.clone();
            if let Some(link) = &self.workers {
                link.set_allowlist(list.clone());
            }
            self.with_runner(move |runner| runner.set_allowlist(list))
                .await;
            self.tell_probes(&new).await;
        }
        if report.applies("workers.host") {
            if let Some(link) = &self.workers {
                // `agent.profile` needs a restart, so the start config's profile still holds.
                let profile = self.config.agent.profile_or_default(Os::current());
                link.set_host_policy(new.workers.host, Some(profile));
            }
        }
        if report.applies("agent.notify") {
            self.notify = new.agent.notify;
        }
        info!(
            applied = ?report.applied,
            restart_agent = ?report.restart_agent,
            "config reloaded"
        );
        self.config = new;
        if report.guard_changed() || allowlist || report.applies("agent.keep_awake") {
            // Take a verdict now, so the new rule shows in the status and can start work at once.
            self.on_sample().await;
        }
        Ok(report)
    }

    /// New `[guard]` overrides on the preset the agent runs with. The device profile is read at
    /// start, so a profile change in the same edit waits for a restart.
    fn apply_guard(&mut self, new: &Config) -> Result<(), String> {
        let mut basis = new.clone();
        basis.agent.profile = self.config.agent.profile;
        let mut policy = basis
            .guard_policy_for(
                Os::current(),
                self.device.cpu_count,
                self.device.mem_total_bytes,
            )
            .map_err(|e| e.to_string())?;
        self.memory_limit = policy.apply_memory_cap(self.memory_cap, self.server_profile);
        self.guard.set_policy(policy);
        Ok(())
    }

    /// The coordinator reads each device's allowlist from its device info to warn about jobs
    /// nobody can run, so the probes learn the new one and the info is sent again.
    async fn tell_probes(&mut self, new: &Config) {
        let cfg = new.agent.clone();
        match blocking_within(&self.probes, PROBE_LIMIT, move |p| p.set_agent_config(cfg)).await {
            Ok(()) => self.on_refresh_device_info().await,
            Err(e) => warn!("could not hand the new allowlist to the probes: {e}"),
        }
    }
}
