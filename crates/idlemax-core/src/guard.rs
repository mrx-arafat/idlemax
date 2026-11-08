//! Domain: the guard turns a metrics sample into Run, Hold, Pause or Kill.
//! Pure; time comes from `MetricsSnapshot::at_ms`. Pause on one breach, kill only after
//! `kill_after_breaches` consecutive kill-level samples, resume after `resume_after_clear`
//! clear samples.

use std::collections::VecDeque;

use crate::config::{GuardPolicy, SwapGrowth};
use crate::protocol::RunningJob;
use crate::types::{GuardAction, GuardDecision, JobStatus, MetricsSnapshot, UnixMs};

#[cfg(test)]
mod tests;

/// Run with no limit; combined with capacity via `GuardAction::max`.
const UNLIMITED: GuardAction = GuardAction::Run {
    slots: u32::MAX,
    mem_mib: u64::MAX,
};

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Hold,
    Pause,
    Kill,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Hold => "hold",
            Level::Pause => "pause",
            Level::Kill => "kill",
        }
    }
}

/// Breaches found in one sample.
#[derive(Debug, Default)]
struct Findings(Vec<(Level, String)>);

impl Findings {
    fn add(&mut self, level: Level, reason: String) {
        self.0.push((level, reason));
    }

    fn has(&self, level: Level) -> bool {
        self.0.iter().any(|(l, _)| *l == level)
    }

    /// Kill-level when `value < kill`, else pause when `value < pause`.
    fn below(&mut self, what: &str, value: f64, limits: (Option<f64>, Option<f64>), unit: &str) {
        self.below_with(Level::Pause, what, value, limits, unit);
    }

    /// Like `below`, with `soft` as the level of the first line.
    fn below_with(
        &mut self,
        soft: Level,
        what: &str,
        value: f64,
        limits: (Option<f64>, Option<f64>),
        unit: &str,
    ) {
        let (first, kill) = limits;
        if let Some(k) = kill.filter(|k| value < *k) {
            self.add(Level::Kill, format!("{what} {value:.1}{unit} < {k}{unit}"));
        } else if let Some(p) = first.filter(|p| value < *p) {
            self.add(soft, format!("{what} {value:.1}{unit} < {p}{unit}"));
        }
    }

    /// Kill-level when `value > kill`, else pause when `value > pause`.
    fn above(&mut self, what: &str, value: f64, limits: (Option<f64>, Option<f64>), unit: &str) {
        let (pause, kill) = limits;
        if let Some(k) = kill.filter(|k| value > *k) {
            self.add(Level::Kill, format!("{what} {value:.1}{unit} > {k}{unit}"));
        } else if let Some(p) = pause.filter(|p| value > *p) {
            self.add(Level::Pause, format!("{what} {value:.1}{unit} > {p}{unit}"));
        }
    }
}

/// CPUs' worth of load the running jobs add: their measured CPU use, paused jobs excluded.
fn own_cpus(running: &[RunningJob]) -> f64 {
    let busy = running.iter().filter(|j| j.status == JobStatus::Running);
    busy.filter_map(|j| j.cpu_pct).sum::<f64>() / 100.0
}

fn mib_limits(pause: Option<u64>, kill: Option<u64>) -> (Option<f64>, Option<f64>) {
    (pause.map(|v| v as f64), kill.map(|v| v as f64))
}

/// Stateful guard for one device: keeps swap history and breach streaks.
#[derive(Debug, Clone)]
pub struct Guard {
    policy: GuardPolicy,
    /// (at_ms, swap_used_bytes), oldest first, trimmed to the longest growth window.
    swap: VecDeque<(UnixMs, u64)>,
    swap_in_high_since: Option<UnixMs>,
    kill_streak: u32,
    clear_streak: u32,
    recovering: bool,
}

impl Guard {
    pub fn new(policy: GuardPolicy) -> Self {
        Self {
            policy,
            swap: VecDeque::new(),
            swap_in_high_since: None,
            kill_streak: 0,
            clear_streak: 0,
            recovering: false,
        }
    }

    pub fn policy(&self) -> &GuardPolicy {
        &self.policy
    }

    /// One verdict per sample. Call once per sample, in time order.
    pub fn evaluate(
        &mut self,
        m: &MetricsSnapshot,
        running: &[RunningJob],
        kill_switch: bool,
        local_pause: bool,
    ) -> GuardDecision {
        let mut found = Findings::default();
        self.check_memory(m, &mut found);
        self.check_swap(m, &mut found);
        self.check_load(m, running, &mut found);
        self.check_environment(m, &mut found);

        let mut reasons: Vec<String> = found
            .0
            .iter()
            .map(|(l, r)| format!("{}: {r}", l.label()))
            .collect();
        let mut resource = self
            .health(&found, &mut reasons)
            .max(self.capacity(running));
        if kill_switch {
            resource = resource.max(GuardAction::Pause);
            reasons.push("pause: kill switch file PAUSE present".into());
        }
        if local_pause {
            resource = resource.max(GuardAction::Pause);
            reasons.push("pause: local pause active".into());
        }
        let (user_active, user) = self.presence(m, &mut reasons);
        GuardDecision {
            action: resource.max(user),
            resource_action: resource,
            user_active,
            reasons,
            at_ms: m.at_ms,
        }
    }

    /// Breach streaks and recovery hysteresis turn findings into a health verdict.
    fn health(&mut self, found: &Findings, reasons: &mut Vec<String>) -> GuardAction {
        let kill_level = found.has(Level::Kill);
        self.kill_streak = if kill_level {
            self.kill_streak.saturating_add(1)
        } else {
            0
        };
        let needed = self.policy.kill_after_breaches.max(1);
        let mut action = if kill_level && self.kill_streak >= needed {
            GuardAction::Kill
        } else if kill_level || found.has(Level::Pause) {
            GuardAction::Pause
        } else {
            UNLIMITED
        };
        if kill_level && action == GuardAction::Pause {
            let streak = self.kill_streak;
            reasons.push(format!(
                "pause: kill-level breach {streak}/{needed}, pausing first"
            ));
        }
        if matches!(action, GuardAction::Pause | GuardAction::Kill) {
            self.recovering = true;
            self.clear_streak = 0;
        } else if self.recovering {
            self.clear_streak = self.clear_streak.saturating_add(1);
            let need = self.policy.resume_after_clear;
            if self.clear_streak >= need {
                self.recovering = false;
            } else {
                action = GuardAction::Hold;
                let clear = self.clear_streak;
                reasons.push(format!("hold: recovering, {clear}/{need} clear samples"));
            }
        }
        if found.has(Level::Hold) {
            action = action.max(GuardAction::Hold);
        }
        action
    }

    /// Job slots and RAM budget left after the running (and paused) jobs.
    fn capacity(&self, running: &[RunningJob]) -> GuardAction {
        let used = u32::try_from(running.len()).unwrap_or(u32::MAX);
        let mem: u64 = running.iter().map(|j| j.mem_mb).sum();
        GuardAction::Run {
            slots: self.policy.max_jobs.saturating_sub(used),
            mem_mib: self.policy.ram_budget_mib.saturating_sub(mem),
        }
    }

    /// User presence: (user_active, verdict). Unknown idle time holds new work, unless the
    /// minimum is 0: then nothing needs waiting for, which is how a host that cannot measure
    /// idle time (a headless Linux box) opts out of the check.
    fn presence(&self, m: &MetricsSnapshot, reasons: &mut Vec<String>) -> (bool, GuardAction) {
        match (self.policy.user_idle_min_secs, m.user_idle_secs) {
            (Some(min), None) if min > 0 => {
                reasons.push("hold: user idle time unknown".into());
                (false, GuardAction::Hold)
            }
            (Some(min), Some(idle)) if idle < min => {
                reasons.push(format!("pause: user active {idle}s ago (< {min}s)"));
                (true, GuardAction::Pause)
            }
            _ => (false, UNLIMITED),
        }
    }

    fn check_memory(&self, m: &MetricsSnapshot, f: &mut Findings) {
        let p = &self.policy;
        if m.mem_total_bytes == 0 {
            f.add(Level::Pause, "memory metrics unavailable".into());
            return;
        }
        let pct = (p.mem_free_pct_pause, p.mem_free_pct_kill);
        f.below("memory free", m.mem_free_pct, pct, "%");
        let avail = mib_limits(p.mem_available_mib_pause, p.mem_available_mib_kill);
        f.below("MemAvailable", mib(m.mem_available_bytes), avail, " MiB");
        if let Some(level) = m.memory_pressure {
            if p.memory_pressure_kill.is_some_and(|k| level >= k) {
                f.add(Level::Kill, format!("memory pressure {level:?}"));
            } else if p.memory_pressure_pause.is_some_and(|k| level >= k) {
                f.add(Level::Pause, format!("memory pressure {level:?}"));
            }
        }
    }

    fn check_swap(&mut self, m: &MetricsSnapshot, f: &mut Findings) {
        self.record_swap(m.at_ms, m.swap_used_bytes);
        let (pause, kill) = (self.policy.swap_growth_pause, self.policy.swap_growth_kill);
        let grown = |g: SwapGrowth| {
            let growth = mib(self.swap_growth(m.at_ms, g.window_secs));
            (growth > g.mib as f64).then(|| {
                format!(
                    "swap +{growth:.0} MiB in {}s > {} MiB",
                    g.window_secs, g.mib
                )
            })
        };
        if let Some(reason) = kill.and_then(grown) {
            f.add(Level::Kill, reason);
        } else if let Some(reason) = pause.and_then(grown) {
            f.add(Level::Pause, reason);
        }
        let used = mib_limits(
            self.policy.swap_used_mib_pause,
            self.policy.swap_used_mib_kill,
        );
        f.above("swap used", mib(m.swap_used_bytes), used, " MiB");
        self.check_swap_in(m, f);
    }

    fn record_swap(&mut self, now: UnixMs, used: u64) {
        if self.swap.back().is_some_and(|(at, _)| *at > now) {
            self.swap.clear(); // clock went backwards
        }
        self.swap.push_back((now, used));
        let windows = [self.policy.swap_growth_pause, self.policy.swap_growth_kill];
        let keep_secs = windows
            .iter()
            .flatten()
            .map(|g| g.window_secs)
            .max()
            .unwrap_or(0);
        let oldest = now.saturating_sub(
            i64::try_from(keep_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
        while self.swap.front().is_some_and(|(at, _)| *at < oldest) {
            self.swap.pop_front();
        }
    }

    /// Bytes swap grew within the window: current minus the lowest sample in it.
    fn swap_growth(&self, now: UnixMs, window_secs: u64) -> u64 {
        let window_ms = i64::try_from(window_secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(1000);
        let from = now.saturating_sub(window_ms);
        let current = self.swap.back().map_or(0, |s| s.1);
        let lowest = self
            .swap
            .iter()
            .filter(|(at, _)| *at >= from)
            .map(|s| s.1)
            .min();
        current.saturating_sub(lowest.unwrap_or(current))
    }

    /// Pause once swap-in stays above the limit for `swap_in_sustain_secs`.
    fn check_swap_in(&mut self, m: &MetricsSnapshot, f: &mut Findings) {
        let Some(limit) = self.policy.swap_in_pages_per_sec_pause else {
            return;
        };
        match m.swap_in_per_sec {
            Some(rate) if rate > limit => {
                let since = *self.swap_in_high_since.get_or_insert(m.at_ms);
                let secs = m.at_ms.saturating_sub(since) / 1000;
                let sustain = i64::try_from(self.policy.swap_in_sustain_secs).unwrap_or(i64::MAX);
                if secs >= sustain {
                    f.add(
                        Level::Pause,
                        format!("swap-in {rate:.0} pages/s > {limit} for {secs}s"),
                    );
                }
            }
            _ => self.swap_in_high_since = None,
        }
    }

    fn check_load(&self, m: &MetricsSnapshot, running: &[RunningJob], f: &mut Findings) {
        let p = &self.policy;
        self.check_load_per_cpu(m, running, f);
        if let Some(v) = m.psi_mem_some_avg10 {
            f.above(
                "PSI memory some avg10",
                v,
                (p.psi_mem_some_pause, None),
                "%",
            );
        }
        if let Some(v) = m.psi_mem_full_avg10 {
            f.above("PSI memory full avg10", v, (None, p.psi_mem_full_kill), "%");
        }
        if let Some(v) = m.psi_io_full_avg10 {
            f.above(
                "PSI io full avg10",
                v,
                (p.psi_io_full_pause, p.psi_io_full_kill),
                "%",
            );
        }
        f.above(
            "load 1m",
            m.load_avg_1m,
            (p.load_1m_pause, p.load_1m_kill),
            "",
        );
    }

    /// Load the machine carries outside the jobs, per CPU. The jobs' own use is taken out so a
    /// busy job never pauses itself, and the limit scales with the number of CPUs.
    fn check_load_per_cpu(&self, m: &MetricsSnapshot, running: &[RunningJob], f: &mut Findings) {
        let (pause, kill) = (
            self.policy.load_per_cpu_pause,
            self.policy.load_per_cpu_kill,
        );
        if pause.is_none() && kill.is_none() {
            return;
        }
        let cpus = m.cpu_count.max(1);
        let others = (m.load_avg_1m - own_cpus(running)).max(0.0);
        let per_cpu = others / f64::from(cpus);
        let reason = |limit: f64| {
            format!(
                "load per CPU {per_cpu:.2} > {limit} ({others:.1} outside the jobs, {cpus} CPUs)"
            )
        };
        if let Some(k) = kill.filter(|k| per_cpu > *k) {
            f.add(Level::Kill, reason(k));
        } else if let Some(p) = pause.filter(|p| per_cpu > *p) {
            f.add(Level::Pause, reason(p));
        }
    }

    fn check_environment(&self, m: &MetricsSnapshot, f: &mut Findings) {
        let p = &self.policy;
        let disk = mib_limits(p.disk_free_mib_pause, p.disk_free_mib_kill);
        if disk != (None, None) {
            if m.disk_total_bytes == 0 {
                f.add(Level::Hold, "disk metrics unavailable".into());
            } else {
                // A pause gives no disk back, so the first line only holds new jobs.
                let free = mib(m.disk_free_bytes);
                f.below_with(Level::Hold, "disk free", free, disk, " MiB");
            }
        }
        if p.require_ac_power {
            match m.on_ac_power {
                Some(false) => f.add(Level::Pause, "on battery".into()),
                None => f.add(Level::Hold, "power source unknown".into()),
                Some(true) => {}
            }
        }
        if let (Some(min), Some(pct)) = (p.min_battery_pct, m.battery_pct) {
            f.below("battery", pct, (Some(min), None), "%");
        }
        if p.pause_on_thermal && m.thermal_limited == Some(true) {
            f.add(Level::Pause, "thermal limited".into());
        }
    }
}
