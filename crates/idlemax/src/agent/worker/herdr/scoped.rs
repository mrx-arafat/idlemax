//! The Herdr host's worker scopes (red team case 10): a pane's shell is moved into a scope
//! of its own before the agent starts, so the agent and whatever it double-forks stay in
//! it, and a stop ends the whole scope. A failed move is logged and the pane runs as before.

use std::path::PathBuf;

use super::{run, HerdrHost};
use crate::agent::worker::host::Launch;
use crate::agent::worker::scope::worker_cgroup;

impl HerdrHost {
    /// Moves the pane's shell into a new worker scope, when this machine has one.
    pub(super) fn adopt(&self, pane: &str, launch: &Launch) {
        let Some(scope) = &self.scope else {
            return;
        };
        let pid = match self.process_info(pane).map(|i| i.shell_pid) {
            Ok(Some(pid)) => pid,
            _ => return tracing::info!(pane, "no shell pid: the worker runs without a scope"),
        };
        let Some((program, args)) = scope.adopt(launch.job, launch.attempt, pid) else {
            return tracing::info!(pane, "no busctl: the worker runs without a scope");
        };
        if let Err(e) = run(self.commands.as_ref(), &program, &args) {
            tracing::info!(pane, error = %e, "the worker runs without a scope");
        }
    }

    /// The worker scope of the pane's shell, read before the pane closes.
    pub(super) fn scope_of(&self, pane: &str) -> Option<PathBuf> {
        self.scope.as_ref()?;
        let pid = self.process_info(pane).ok()?.shell_pid?;
        worker_cgroup(pid)
    }
}
