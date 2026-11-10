//! Domain: why the guard held work, as a short stable vocabulary (pure).
//!
//! The guard words its reasons for a person (`pause: memory pressure Warn`). The report keeps
//! only the category, so a reworded message never changes what an old row means.

use serde::{Deserialize, Serialize};

use crate::types::{GuardAction, GuardDecision};

/// Why the guard held new work. The order is the precedence: when one verdict has several
/// reasons, the first one in this list is its main reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    PauseFile,
    LocalPause,
    /// A coordinator pause (`idlemax pause`) held the device.
    Paused,
    UserActive,
    /// The host cannot read how long the user has been idle, so the guard holds new work.
    IdleUnknown,
    Battery,
    Thermal,
    Memory,
    Disk,
    Load,
    Recovering,
    Other,
}

impl HoldReason {
    pub const ALL: [HoldReason; 12] = [
        HoldReason::PauseFile,
        HoldReason::LocalPause,
        HoldReason::Paused,
        HoldReason::UserActive,
        HoldReason::IdleUnknown,
        HoldReason::Battery,
        HoldReason::Thermal,
        HoldReason::Memory,
        HoldReason::Disk,
        HoldReason::Load,
        HoldReason::Recovering,
        HoldReason::Other,
    ];

    /// The stored form, the same text serde writes.
    pub fn as_str(self) -> &'static str {
        match self {
            HoldReason::PauseFile => "pause_file",
            HoldReason::LocalPause => "local_pause",
            HoldReason::Paused => "paused",
            HoldReason::UserActive => "user_active",
            HoldReason::IdleUnknown => "idle_unknown",
            HoldReason::Battery => "battery",
            HoldReason::Thermal => "thermal",
            HoldReason::Memory => "memory",
            HoldReason::Disk => "disk",
            HoldReason::Load => "load",
            HoldReason::Recovering => "recovering",
            HoldReason::Other => "other",
        }
    }

    /// What the report shows a person.
    pub fn label(self) -> &'static str {
        match self {
            HoldReason::PauseFile => "the PAUSE file",
            HoldReason::LocalPause => "a local pause",
            HoldReason::Paused => "a pause you set",
            HoldReason::UserActive => "you using the machine",
            HoldReason::IdleUnknown => "idle time it cannot read",
            HoldReason::Battery => "battery",
            HoldReason::Thermal => "thermal limits",
            HoldReason::Memory => "memory pressure",
            HoldReason::Disk => "low disk space",
            HoldReason::Load => "load",
            HoldReason::Recovering => "recovering after a breach",
            HoldReason::Other => "other guard rules",
        }
    }

    pub fn parse(text: &str) -> Option<HoldReason> {
        HoldReason::ALL.into_iter().find(|r| r.as_str() == text)
    }
}

/// The category of one guard reason line (`pause: swap +300 MiB in 60s > 200 MiB`), or None
/// for a line that only annotates another one.
pub fn classify(reason: &str) -> Option<HoldReason> {
    let text = reason
        .split_once(": ")
        .map_or(reason, |(_, rest)| rest)
        .to_ascii_lowercase();
    if text.contains("pausing first") {
        return None;
    }
    Some(if text.contains("kill switch") {
        HoldReason::PauseFile
    } else if text.contains("local pause") {
        HoldReason::LocalPause
    } else if text.starts_with("recovering") {
        HoldReason::Recovering
    } else if text.contains("idle time unknown") {
        HoldReason::IdleUnknown
    } else if text.starts_with("user ") {
        HoldReason::UserActive
    } else if text.contains("battery") || text.contains("power source") {
        HoldReason::Battery
    } else if text.contains("thermal") {
        HoldReason::Thermal
    } else if text.starts_with("disk") {
        HoldReason::Disk
    } else if text.contains("memory") || text.contains("memavailable") || text.starts_with("swap") {
        HoldReason::Memory
    } else if text.starts_with("load") || text.starts_with("psi") {
        HoldReason::Load
    } else {
        HoldReason::Other
    })
}

/// What the report keeps of one guard verdict: whether new work was held, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardState {
    pub held: bool,
    /// Sorted by precedence, no duplicates; empty when not held.
    pub reasons: Vec<HoldReason>,
}

impl GuardState {
    /// Only Hold, Pause and Kill hold work. Run with no slots left is a full machine, which
    /// is what the jobs ask for.
    pub fn of(decision: &GuardDecision) -> Self {
        let held = matches!(
            decision.action,
            GuardAction::Hold | GuardAction::Pause | GuardAction::Kill
        );
        if !held {
            return Self {
                held,
                reasons: Vec::new(),
            };
        }
        let mut reasons: Vec<HoldReason> = decision
            .reasons
            .iter()
            .filter_map(|r| classify(r))
            .collect();
        reasons.sort();
        reasons.dedup();
        if reasons.is_empty() {
            reasons.push(HoldReason::Other);
        }
        Self { held, reasons }
    }

    /// This verdict with `reason` added: the device is held whatever it said before.
    pub fn with_reason(&self, reason: HoldReason) -> Self {
        let mut reasons = self.reasons.clone();
        reasons.push(reason);
        reasons.sort();
        reasons.dedup();
        Self {
            held: true,
            reasons,
        }
    }

    /// The stored form of `reasons`: stored names joined by commas.
    pub fn reasons_text(&self) -> String {
        let names: Vec<&str> = self.reasons.iter().map(|r| r.as_str()).collect();
        names.join(",")
    }

    /// Rebuilds a state from its stored columns; an unknown name reads as `Other`.
    pub fn from_stored(held: bool, reasons: &str) -> Self {
        let mut reasons: Vec<HoldReason> = reasons
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| HoldReason::parse(s).unwrap_or(HoldReason::Other))
            .collect();
        reasons.sort();
        reasons.dedup();
        Self { held, reasons }
    }
}
