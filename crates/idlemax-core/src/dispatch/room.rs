//! Domain: the worker room of one device: the line state of D14 (run, hold, shed, emergency),
//! its reason and the number of open worker slots, from one metrics sample. Pure; time comes
//! from `MetricsSnapshot::at_ms`.
//!
//! It never reads user idle time, the macOS memory pressure level or the power source (D4,
//! A16), so a worker may start while the owner types and while pressure is `Warn`.

use serde::{Deserialize, Serialize};

use crate::config::{DispatcherConfig, GuardPolicy};
use crate::types::{GuardAction, GuardDecision, MetricsSnapshot, UnixMs};

#[cfg(test)]
mod tests;

/// D14 says "for the last minute".
pub const SWAP_IN_WINDOW_MS: i64 = 60_000;

/// The line state, ordered by severity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    Run,
    #[default]
    Hold,
    Shed,
    Emergency,
}

impl LineState {
    pub fn as_str(self) -> &'static str {
        match self {
            LineState::Run => "run",
            LineState::Hold => "hold",
            LineState::Shed => "shed",
            LineState::Emergency => "emergency",
        }
    }
}

/// The room of one device at one sample.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomDecision {
    pub state: LineState,
    /// The reading that set the state, with its value and line.
    pub reason: String,
    /// Workers that fit under the hold line; 0 unless the state is `run`.
    pub open_slots: u32,
    pub at_ms: UnixMs,
}

impl RoomDecision {
    /// A room that takes nothing, for when there is no reading.
    pub fn hold(reason: impl Into<String>, at_ms: UnixMs) -> Self {
        Self {
            state: LineState::Hold,
            reason: format!("hold: {}", reason.into()),
            open_slots: 0,
            at_ms,
        }
    }
}

/// The room in the guard's words, for the dispatcher's own jobs (A16): `run` with an open slot
/// allows one job; `run` without one and `hold` start nothing; `shed` and `emergency` kill.
/// No room is `hold`.
pub fn lines_verdict(room: Option<&RoomDecision>, at_ms: UnixMs) -> GuardDecision {
    let (action, reason) = match room {
        None => (
            GuardAction::Hold,
            "hold: this agent reports no worker room".to_string(),
        ),
        Some(r) => {
            let action = match r.state {
                LineState::Run if r.open_slots > 0 => GuardAction::Run {
                    slots: 1,
                    mem_mib: u64::MAX,
                },
                LineState::Run | LineState::Hold => GuardAction::Hold,
                LineState::Shed | LineState::Emergency => GuardAction::Kill,
            };
            let reason = if r.state == LineState::Run && r.open_slots == 0 {
                format!("hold: no open worker slot ({})", r.reason)
            } else {
                r.reason.clone()
            };
            (action, reason)
        }
    };
    GuardDecision {
        memory_limit: None,
        action,
        resource_action: action,
        user_active: false,
        reasons: vec![format!("lines: {reason}")],
        at_ms,
    }
}

/// The numbers of one device: the `[dispatcher]` lines plus the two guard values A17 names.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomPolicy {
    pub hold_mem_used_pct: f64,
    pub shed_mem_used_pct: f64,
    pub emergency_mem_used_pct: f64,
    pub hold_disk_free_mib: u64,
    pub shed_disk_free_mib: u64,
    pub emergency_disk_free_mib: u64,
    pub swap_in_low_pages_per_sec: f64,
    pub swap_in_high_pages_per_sec: f64,
    pub worker_mem_mib: u64,
    pub min_battery_pct: Option<f64>,
    pub pause_on_thermal: bool,
}

impl RoomPolicy {
    pub fn new(cfg: &DispatcherConfig, guard: &GuardPolicy) -> Self {
        Self {
            hold_mem_used_pct: cfg.hold_mem_used_pct,
            shed_mem_used_pct: cfg.shed_mem_used_pct,
            emergency_mem_used_pct: cfg.emergency_mem_used_pct,
            hold_disk_free_mib: cfg.hold_disk_free_mib,
            shed_disk_free_mib: cfg.shed_disk_free_mib,
            emergency_disk_free_mib: cfg.emergency_disk_free_mib,
            swap_in_low_pages_per_sec: cfg.swap_in_low_pages_per_sec,
            swap_in_high_pages_per_sec: cfg.swap_in_high_pages_per_sec,
            worker_mem_mib: cfg.worker_mem_mib,
            min_battery_pct: guard.min_battery_pct,
            pause_on_thermal: guard.pause_on_thermal,
        }
    }
}

/// The worker room of one device: keeps when swap-in turned low or high.
#[derive(Debug, Clone)]
pub struct WorkerRoom {
    policy: RoomPolicy,
    low_since: Option<UnixMs>,
    high_since: Option<UnixMs>,
}

/// The readings that set a state, worst first after sorting.
type Findings = Vec<(LineState, String)>;

impl WorkerRoom {
    pub fn new(policy: RoomPolicy) -> Self {
        Self {
            policy,
            low_since: None,
            high_since: None,
        }
    }

    pub fn policy(&self) -> &RoomPolicy {
        &self.policy
    }

    /// Swaps the numbers (a config reload) and keeps both swap-in marks.
    pub fn set_policy(&mut self, policy: RoomPolicy) {
        self.policy = policy;
    }

    /// One decision per sample, computed fresh; call in time order.
    pub fn evaluate(&mut self, m: &MetricsSnapshot) -> RoomDecision {
        let mut found = Findings::new();
        let used = self.check_memory(m, &mut found);
        self.check_disk(m, &mut found);
        self.check_swap_in(m, &mut found);
        self.check_environment(m, &mut found);
        let state = found
            .iter()
            .map(|(s, _)| *s)
            .max()
            .unwrap_or(LineState::Run);
        if state != LineState::Run {
            let texts: Vec<&str> = found
                .iter()
                .filter(|(s, _)| *s == state)
                .map(|(_, t)| t.as_str())
                .collect();
            return RoomDecision {
                state,
                reason: format!("{}: {}", state.as_str(), texts.join("; ")),
                open_slots: 0,
                at_ms: m.at_ms,
            };
        }
        let used = used.unwrap_or(100.0);
        RoomDecision {
            state,
            reason: self.run_reason(m, used),
            open_slots: self.open_slots(m, used),
            at_ms: m.at_ms,
        }
    }

    fn run_reason(&self, m: &MetricsSnapshot, used: f64) -> String {
        let swap = m
            .swap_in_per_sec
            .map_or_else(|| "unknown".into(), |r| format!("{} pages/s", num(r)));
        format!(
            "run: memory {}% used (hold at {}%), swap-in {swap}, disk {} MiB free",
            num(used),
            num(self.policy.hold_mem_used_pct),
            mib(m.disk_free_bytes)
        )
    }

    /// Whole workers of `worker_mem_mib` between the reading and the hold line (D13).
    fn open_slots(&self, m: &MetricsSnapshot, used: f64) -> u32 {
        let total_mib = m.mem_total_bytes as f64 / (1024.0 * 1024.0);
        let room_mib = (self.policy.hold_mem_used_pct - used).max(0.0) / 100.0 * total_mib;
        let slots = (room_mib / self.policy.worker_mem_mib.max(1) as f64).floor();
        if slots >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            slots as u32
        }
    }

    fn check_memory(&self, m: &MetricsSnapshot, f: &mut Findings) -> Option<f64> {
        let p = &self.policy;
        let Some(used) = m.mem_used_pct.filter(|_| m.mem_total_bytes > 0) else {
            f.push((
                LineState::Hold,
                "memory used percent is not read on this system".into(),
            ));
            return None;
        };
        let lines = [
            (LineState::Emergency, p.emergency_mem_used_pct, "emergency"),
            (LineState::Shed, p.shed_mem_used_pct, "shed"),
            (LineState::Hold, p.hold_mem_used_pct, "hold"),
        ];
        if let Some((state, line, name)) = lines.into_iter().find(|(_, line, _)| used >= *line) {
            f.push((
                state,
                format!("memory {}% used ({name} at {}%)", num(used), num(line)),
            ));
        }
        Some(used)
    }

    fn check_disk(&self, m: &MetricsSnapshot, f: &mut Findings) {
        let p = &self.policy;
        if m.disk_total_bytes == 0 {
            f.push((LineState::Hold, "disk reading unavailable".into()));
            return;
        }
        let free = mib(m.disk_free_bytes);
        let lines = [
            (LineState::Emergency, p.emergency_disk_free_mib, "emergency"),
            (LineState::Shed, p.shed_disk_free_mib, "shed"),
            (LineState::Hold, p.hold_disk_free_mib, "hold"),
        ];
        if let Some((state, line, name)) = lines.into_iter().find(|(_, line, _)| free < *line) {
            f.push((
                state,
                format!("disk {free} MiB free ({name} below {line} MiB)"),
            ));
        }
    }

    /// Shed when swap-in stayed high for the window; hold until it stayed low for it.
    fn check_swap_in(&mut self, m: &MetricsSnapshot, f: &mut Findings) {
        let p = &self.policy;
        let now = m.at_ms;
        match m.swap_in_per_sec {
            Some(rate) if rate <= p.swap_in_low_pages_per_sec => {
                self.low_since.get_or_insert(now);
                self.high_since = None;
            }
            Some(rate) if rate > p.swap_in_high_pages_per_sec => {
                self.high_since.get_or_insert(now);
                self.low_since = None;
            }
            _ => {
                self.low_since = None;
                self.high_since = None;
            }
        }
        for mark in [&mut self.low_since, &mut self.high_since]
            .into_iter()
            .flatten()
        {
            if *mark > now {
                *mark = now; // the clock stepped back
            }
        }
        let rate = m
            .swap_in_per_sec
            .map_or_else(|| "not read".into(), |r| format!("{} pages/s", num(r)));
        if let Some(since) = self.high_since {
            if now - since >= SWAP_IN_WINDOW_MS {
                let high = num(p.swap_in_high_pages_per_sec);
                f.push((
                    LineState::Shed,
                    format!("swap-in {rate} stayed above {high} for a minute"),
                ));
                return;
            }
        }
        let low_for_a_minute = self.low_since.is_some_and(|s| now - s >= SWAP_IN_WINDOW_MS);
        if !low_for_a_minute {
            let low = num(p.swap_in_low_pages_per_sec);
            f.push((
                LineState::Hold,
                format!("swap-in {rate} has not been at or under {low} for the last minute"),
            ));
        }
    }

    /// The battery floor and thermal throttling of the guard hold workers too (A17).
    fn check_environment(&self, m: &MetricsSnapshot, f: &mut Findings) {
        let p = &self.policy;
        if let (Some(floor), Some(pct)) = (p.min_battery_pct, m.battery_pct) {
            if pct < floor {
                f.push((
                    LineState::Hold,
                    format!("battery {}% (floor {}%)", num(pct), num(floor)),
                ));
            }
        }
        if p.pause_on_thermal && m.thermal_limited == Some(true) {
            f.push((LineState::Hold, "thermal limited".into()));
        }
    }
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// 90.0 as "90", 89.94 as "89.9".
fn num(v: f64) -> String {
    let text = format!("{v:.1}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_string)
}
