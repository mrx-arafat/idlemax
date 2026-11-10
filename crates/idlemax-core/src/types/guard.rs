//! Domain: the guard verdict for a device and how two verdicts combine.

use serde::{Deserialize, Serialize};

use super::UnixMs;

/// Guard verdict, ordered by severity: Run < Hold < Pause < Kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardAction {
    /// Running jobs may run (paused ones resume). Up to `slots` more jobs may start
    /// with at most `mem_mib` of `mem_mb` between them.
    Run { slots: u32, mem_mib: u64 },
    /// Start nothing new; leave running and paused jobs as they are.
    #[default]
    Hold,
    /// SIGSTOP running jobs; start nothing.
    Pause,
    /// SIGTERM then SIGKILL running jobs; start nothing.
    Kill,
}

impl GuardAction {
    pub fn severity(self) -> u8 {
        match self {
            GuardAction::Run { .. } => 0,
            GuardAction::Hold => 1,
            GuardAction::Pause => 2,
            GuardAction::Kill => 3,
        }
    }

    /// The more severe of two actions; on a tie between two Runs, the smaller one.
    pub fn max(self, other: GuardAction) -> GuardAction {
        match (self, other) {
            (
                GuardAction::Run {
                    slots: a,
                    mem_mib: m,
                },
                GuardAction::Run {
                    slots: b,
                    mem_mib: n,
                },
            ) => GuardAction::Run {
                slots: a.min(b),
                mem_mib: m.min(n),
            },
            _ if other.severity() > self.severity() => other,
            _ => self,
        }
    }

    pub fn may_start(self) -> bool {
        matches!(self, GuardAction::Run { slots, mem_mib } if slots > 0 && mem_mib > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<MemoryLimit>,
    /// Combined verdict: use for starting jobs and for jobs with on_user_active = pause.
    pub action: GuardAction,
    /// Verdict from machine health and the kill switch only, ignoring user presence.
    /// Jobs with on_user_active = continue follow this; on_user_active = kill jobs are
    /// killed when `user_active` is true.
    pub resource_action: GuardAction,
    pub user_active: bool,
    pub reasons: Vec<String>,
    pub at_ms: UnixMs,
}

/// Why the host-sized or configured RAM budget was reduced by the service's cgroup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryLimit {
    pub configured_mib: u64,
    pub effective_mib: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroup_max_bytes: Option<u64>,
}

impl std::fmt::Display for MemoryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some(cap) = self.cgroup_max_bytes else {
            return write!(f, "cgroup memory limit unknown; reference fallback caps capacity at 1 job / 512 MiB (effective RAM {} MiB)", self.effective_mib);
        };
        write!(f, "RAM budget clamped from {} to {} MiB by effective cgroup memory limit={} bytes ({} MiB reserved for agent)",
            self.configured_mib, self.effective_mib, cap, crate::config::AGENT_OVERHEAD_MIB)
    }
}
