//! Domain: opt-in idle-sleep prevention while useful work waits for an idle machine.

use serde::{Deserialize, Serialize};

use crate::types::{GuardAction, GuardDecision};

/// Whether the agent may prevent idle system sleep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAwake {
    #[default]
    Off,
    Ac,
}

/// A reason suitable for the local status, and whether to hold the OS assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub hold: bool,
    pub reason: &'static str,
}

/// User activity alone may wait for idle without allowing system sleep to interrupt it.
pub fn decide(
    setting: KeepAwake,
    on_ac: Option<bool>,
    running: bool,
    queued: bool,
    guard: &GuardDecision,
    paused: bool,
) -> Decision {
    let reason = if setting == KeepAwake::Off {
        "off (disabled)"
    } else if on_ac != Some(true) {
        if on_ac == Some(false) {
            "not holding (on battery)"
        } else {
            "not holding (power source unknown)"
        }
    } else if paused || !matches!(guard.resource_action, GuardAction::Run { .. }) {
        "not holding (guard or control holds work)"
    } else if !matches!(guard.action, GuardAction::Run { .. }) && !guard.user_active {
        "not holding (user idle time unknown)"
    } else if running {
        return Decision {
            hold: true,
            reason: "holding (running work, on AC)",
        };
    } else if queued {
        return Decision {
            hold: true,
            reason: "holding (queued work, on AC)",
        };
    } else {
        "not holding (no eligible work)"
    };
    Decision {
        hold: false,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ac_work_with_clear_resources_can_keep_the_machine_awake() {
        let run = GuardAction::Run {
            slots: 1,
            mem_mib: 256,
        };
        let mut guard = GuardDecision {
            action: GuardAction::Pause,
            resource_action: run,
            user_active: true,
            ..Default::default()
        };
        let decision = |setting, ac, running, queued, guard: &GuardDecision, paused| {
            decide(setting, ac, running, queued, guard, paused).hold
        };
        assert!(decision(
            KeepAwake::Ac,
            Some(true),
            false,
            true,
            &guard,
            false
        ));
        guard.action = run;
        guard.user_active = false;
        assert!(decision(
            KeepAwake::Ac,
            Some(true),
            true,
            false,
            &guard,
            false
        ));
        assert!(!decision(
            KeepAwake::Off,
            Some(true),
            true,
            true,
            &guard,
            false
        ));
        for ac in [Some(false), None] {
            assert!(!decision(KeepAwake::Ac, ac, true, true, &guard, false));
        }
        assert!(!decision(
            KeepAwake::Ac,
            Some(true),
            false,
            false,
            &guard,
            false
        ));
        assert!(!decision(
            KeepAwake::Ac,
            Some(true),
            true,
            true,
            &guard,
            true
        ));
        for action in [GuardAction::Hold, GuardAction::Pause, GuardAction::Kill] {
            guard.resource_action = action;
            guard.user_active = true;
            assert!(!decision(
                KeepAwake::Ac,
                Some(true),
                true,
                true,
                &guard,
                false
            ));
        }
        guard.resource_action = run;
        guard.action = GuardAction::Hold;
        guard.user_active = false;
        assert!(!decision(
            KeepAwake::Ac,
            Some(true),
            false,
            true,
            &guard,
            false
        ));
    }
}
