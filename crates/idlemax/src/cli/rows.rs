//! Delivery: turns API views into display cells, shared by CLI tables and the TUI.

use idlemax_core::capacity::{AccountCapacity, CapacityState};
use idlemax_core::protocol::DeviceView;
use idlemax_core::quota::QuotaView;
use idlemax_core::schedule::{Decision, RunResult, ScheduleView};
use idlemax_core::types::{
    Event, GuardAction, GuardDecision, Job, QuotaWindowKind, Recommendation, UnixMs,
};
use serde::Serialize;

pub const DEVICE_HEADER: [&str; 12] = [
    "DEVICE", "STATE", "SLOTS", "RAM", "WHY", "IDLE", "CPU", "LOAD", "MEM FREE", "SWAP", "DISK",
    "POWER",
];
pub const JOB_HEADER: [&str; 9] = [
    "ID", "NAME", "STATUS", "DEVICE", "COST", "PRIO", "TRIES", "AGE", "RESULT",
];
pub const ACCOUNT_HEADER: [&str; 6] = ["ACCOUNT", "SPEND", "LEFT", "RESETS", "WINDOW", "WHY"];
pub const QUOTA_HEADER: [&str; 8] = [
    "ACCOUNT", "WINDOW", "USED", "RESERVE", "HEADROOM", "RESETS", "SPEND", "WHY",
];
pub const ADVICE_HEADER: [&str; 7] = ["ID", "KIND", "TITLE", "CONF", "SOURCE", "STATUS", "AGE"];
pub const EVENT_HEADER: [&str; 5] = ["TIME", "KIND", "DEVICE", "JOB", "MESSAGE"];
pub const SCHEDULE_HEADER: [&str; 5] = ["ID", "NAME", "EVERY", "STATE", "LAST RESULT"];

pub fn device_cells(d: &DeviceView, now_ms: UnixMs) -> Vec<String> {
    let m = d.metrics.as_ref();
    let (slots, ram, why) = capacity_cells(d);
    vec![
        d.info.id.clone(),
        device_state(d, now_ms),
        slots,
        ram,
        why,
        opt(m.and_then(|m| m.user_idle_secs), |s| {
            dur(i64::try_from(s).unwrap_or(i64::MAX).saturating_mul(1000))
        }),
        opt(m, |m| format!("{:.0}%", m.cpu_busy_pct)),
        opt(m, |m| format!("{:.2}", m.load_avg_1m)),
        opt(m, |m| {
            format!("{:.0}% {}", m.mem_free_pct, gib(m.mem_available_bytes))
        }),
        opt(m, |m| gib(m.swap_used_bytes)),
        opt(m, |m| gib(m.disk_free_bytes)),
        opt(m, power),
    ]
}

/// Slots, RAM budget and the reason it is not using more, as the coordinator worked them
/// out. A coordinator that predates `capacity` leaves only the running count.
fn capacity_cells(d: &DeviceView) -> (String, String, String) {
    let Some(c) = &d.capacity else {
        return (d.running.len().to_string(), "-".into(), "-".into());
    };
    if c.state == CapacityState::Offline {
        return ("-".into(), "-".into(), "-".into());
    }
    let slots = match c.slots_allowed {
        Some(allowed) => format!("{}/{allowed}", c.slots_used),
        None => c.slots_used.to_string(),
    };
    let ram = match c.mem_allowed_mib {
        Some(allowed) => format!("{}/{}", mib(c.mem_used_mib), mib(allowed)),
        None => mib(c.mem_used_mib),
    };
    (slots, ram, c.why.clone())
}

/// "512M", "1.5G".
fn mib(v: u64) -> String {
    if v >= 1024 {
        format!("{:.1}G", v as f64 / 1024.0)
    } else {
        format!("{v}M")
    }
}

/// An AI account's quota: whether idlemax may spend it, how much, and until when.
pub fn account_cells(a: &AccountCapacity, now_ms: UnixMs) -> Vec<String> {
    let spend = match (a.paused, a.spendable) {
        (true, _) => "paused",
        (false, true) => "yes",
        (false, false) => "no",
    };
    vec![
        a.account.clone(),
        spend.into(),
        a.headroom_pct.map_or_else(|| "-".into(), pct),
        a.resets_at_ms
            .map_or_else(|| "-".into(), |t| rel(t, now_ms)),
        a.window.map_or_else(|| "-".into(), window_label),
        a.why
            .as_deref()
            .map_or_else(|| "-".into(), |w| trunc(w, 60)),
    ]
}

/// One sentence for a narrow place: "claude: spendable, 31% left (week), resets in 3h00m".
pub fn account_line(a: &AccountCapacity, now_ms: UnixMs) -> String {
    let may = match (a.paused, a.spendable, a.why.as_deref()) {
        (true, ..) => "paused by you".to_string(),
        (false, true, _) => "spendable".to_string(),
        (false, false, Some(why)) => format!("not spendable now ({why})"),
        (false, false, None) => "not spendable now".to_string(),
    };
    let left = a.headroom_pct.map_or_else(
        || "quota left unknown".to_string(),
        |p| format!("{} left", pct(p)),
    );
    let window = a
        .window
        .map_or_else(String::new, |w| format!(" ({})", window_label(w)));
    let resets = a
        .resets_at_ms
        .map_or_else(String::new, |t| format!(", resets {}", rel(t, now_ms)));
    format!("{}: {may}, {left}{window}{resets}", a.account)
}

fn device_state(d: &DeviceView, now_ms: UnixMs) -> String {
    if !d.online {
        return format!("offline {}", rel(d.last_seen_ms, now_ms));
    }
    match (d.paused, d.local_paused) {
        (true, _) => "paused".into(),
        (false, true) => "local-paused".into(),
        (false, false) => "online".into(),
    }
}

fn power(m: &idlemax_core::types::MetricsSnapshot) -> String {
    let batt = m
        .battery_pct
        .map(|p| format!(" {p:.0}%"))
        .unwrap_or_default();
    match m.on_ac_power {
        Some(true) => format!("ac{batt}"),
        Some(false) => format!("battery{batt}"),
        None => "-".into(),
    }
}

/// "run 2", "pause: disk free ...", "pause (kill: ...)". Guard reasons carry
/// their own level prefix; it is shown once.
pub fn guard_summary(g: &GuardDecision) -> String {
    let (word, action) = match g.action {
        GuardAction::Run { slots, .. } => ("run", format!("run {slots}")),
        GuardAction::Hold => ("hold", "hold".into()),
        GuardAction::Pause => ("pause", "pause".into()),
        GuardAction::Kill => ("kill", "kill".into()),
    };
    let at_level = |r: &&String| r.strip_prefix(word).is_some_and(|r| r.starts_with(": "));
    let Some(reason) = g.reasons.iter().find(at_level).or(g.reasons.first()) else {
        return action;
    };
    match reason.strip_prefix(word).and_then(|r| r.strip_prefix(": ")) {
        Some(rest) => format!("{action}: {}", trunc(rest, 40)),
        None => format!("{action} ({})", trunc(reason, 40)),
    }
}

pub fn job_cells(j: &Job, now_ms: UnixMs) -> Vec<String> {
    vec![
        j.id.to_string(),
        trunc(&j.spec.name, 32),
        j.status.to_string(),
        j.device.clone().unwrap_or_else(|| "-".into()),
        match &j.spec.ai_account {
            Some(acct) => format!("ai:{acct}"),
            None => snake(&j.spec.cost),
        },
        j.spec.priority.to_string(),
        format!("{}/{}", j.attempts, j.spec.effective_max_attempts()),
        rel(j.created_ms, now_ms),
        job_result(j, now_ms),
    ]
}

fn job_result(j: &Job, now_ms: UnixMs) -> String {
    trunc(&job_result_full(j, now_ms), 48)
}

/// The job's last error, else its exit code or how long it has run; "-" when none applies.
pub fn job_result_full(j: &Job, now_ms: UnixMs) -> String {
    if let Some(err) = &j.last_error {
        return err.clone();
    }
    match (j.exit_code, j.started_ms, j.finished_ms) {
        (Some(code), _, _) => format!("exit {code}"),
        (None, Some(start), None) => format!("running {}", dur(now_ms - start)),
        _ => "-".into(),
    }
}

pub fn quota_cells(q: &QuotaView, paused: bool, now_ms: UnixMs) -> Vec<String> {
    let spend = match (paused, q.spendable) {
        (true, _) => "paused",
        (false, true) => "yes",
        (false, false) => "no",
    };
    vec![
        q.account.clone(),
        q.window.map_or_else(|| "-".into(), window_label),
        opt(q.used_pct, pct),
        pct(q.reserve_pct),
        pct(q.headroom_pct),
        q.resets_at_ms
            .map_or_else(|| "-".into(), |t| rel(t, now_ms)),
        spend.into(),
        trunc(&q.reasons.join("; "), 60),
    ]
}

pub fn window_label(w: QuotaWindowKind) -> String {
    match w {
        QuotaWindowKind::Rolling { minutes } if minutes % 60 == 0 => format!("{}h", minutes / 60),
        QuotaWindowKind::Rolling { minutes } => format!("{minutes}m"),
        QuotaWindowKind::Weekly => "week".into(),
    }
}

pub fn advice_cells(r: &Recommendation, now_ms: UnixMs) -> Vec<String> {
    vec![
        r.id.to_string(),
        snake(&r.kind),
        trunc(&r.title, 60),
        format!("{:.2}", r.confidence),
        snake(&r.source),
        snake(&r.status),
        rel(r.at_ms, now_ms),
    ]
}

pub fn event_cells(e: &Event, zone: Zone) -> Vec<String> {
    vec![
        clock(e.at_ms, zone),
        snake(&e.kind),
        e.device.clone().unwrap_or_else(|| "-".into()),
        e.job.map_or_else(|| "-".into(), |j| j.to_string()),
        e.message.clone(),
    ]
}

pub fn schedule_cells(v: &ScheduleView, now_ms: UnixMs) -> Vec<String> {
    vec![
        v.schedule.id.to_string(),
        trunc(&v.schedule.spec.name, 32),
        v.schedule.every.to_string(),
        schedule_state(v, now_ms),
        last_result(v.last_result.as_ref(), now_ms),
    ]
}

/// What the schedule is doing: "job 41 queued", "next in 5h05m", "due now", "paused".
pub fn schedule_state(v: &ScheduleView, now_ms: UnixMs) -> String {
    match (v.decision, v.schedule.paused) {
        (Decision::Running { job, status }, false) => format!("job {job} {status}"),
        (Decision::Running { job, status }, true) => format!("paused, job {job} still {status}"),
        (Decision::Paused, _) => "paused".into(),
        (Decision::Wait { until_ms }, _) => format!("next {}", rel(until_ms, now_ms)),
        (Decision::Fire, _) => "due now".into(),
    }
}

/// "succeeded 3h00m ago", "failed (exit 2) 1m ago"; "-" before the first run ends.
fn last_result(r: Option<&RunResult>, now_ms: UnixMs) -> String {
    let Some(r) = r else { return "-".into() };
    let detail = match (&r.error, r.exit_code) {
        (Some(e), _) => format!(" ({})", trunc(e, 30)),
        (None, Some(code)) if code != 0 => format!(" (exit {code})"),
        _ => String::new(),
    };
    let at = r
        .finished_ms
        .map_or_else(String::new, |t| format!(" {}", rel(t, now_ms)));
    format!("{}{detail}{at}", r.status)
}

/// Name of a unit serde enum, e.g. `CostClass::CpuLight` -> "cpu_light".
pub fn snake<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "?".into(),
    }
}

/// "45s", "12m", "3h05m", "2d4h".
pub fn dur(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
    }
}

/// "3m ago" or "in 2h05m"; "-" for an unset time.
pub fn rel(at_ms: UnixMs, now_ms: UnixMs) -> String {
    if at_ms <= 0 {
        "-".into()
    } else if at_ms <= now_ms {
        format!("{} ago", dur(now_ms - at_ms))
    } else {
        format!("in {}", dur(at_ms - now_ms))
    }
}

/// The zone wall-clock times are shown in: the machine's own, or UTC for output that
/// must not depend on where it is rendered (snapshot tests).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Zone {
    #[default]
    Local,
    Utc,
}

/// Wall-clock time in `zone`, "11-05 03:12:09".
pub fn clock(at_ms: UnixMs, zone: Zone) -> String {
    const FORMAT: &str = "%m-%d %H:%M:%S";
    chrono::DateTime::from_timestamp_millis(at_ms).map_or_else(
        || "-".into(),
        |t| match zone {
            Zone::Local => t.with_timezone(&chrono::Local).format(FORMAT).to_string(),
            Zone::Utc => t.format(FORMAT).to_string(),
        },
    )
}

pub fn gib(bytes: u64) -> String {
    format!("{:.1}G", bytes as f64 / f64::from(1u32 << 30))
}

fn pct(v: f64) -> String {
    format!("{v:.0}%")
}

fn opt<T>(v: Option<T>, f: impl FnOnce(T) -> String) -> String {
    v.map_or_else(|| "-".into(), f)
}

pub fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use idlemax_core::schedule::{Decision, RunResult, Schedule};
    use idlemax_core::types::{JobSpec, JobStatus};

    use super::*;

    fn schedule_view(decision: Decision, last: Option<RunResult>, paused: bool) -> ScheduleView {
        ScheduleView {
            schedule: Schedule {
                id: 3,
                spec: JobSpec {
                    name: "repo-health".into(),
                    ..JobSpec::default()
                },
                every: "1d".parse().unwrap(),
                paused,
                created_ms: 0,
                last_fired_ms: None,
                last_job: None,
            },
            decision,
            last_result: last,
        }
    }

    #[test]
    fn schedule_cells_say_what_it_waits_for_and_how_the_last_run_ended() {
        const NOW: UnixMs = 1_800_000_000_000;
        let failed = RunResult {
            status: JobStatus::Failed,
            exit_code: Some(2),
            error: None,
            finished_ms: Some(NOW - 3_600_000),
        };
        let ok = RunResult {
            status: JobStatus::Succeeded,
            exit_code: Some(0),
            error: None,
            finished_ms: Some(NOW - 60_000),
        };
        let cells = |v: &ScheduleView| schedule_cells(v, NOW)[3..].to_vec();
        let wait = Decision::Wait {
            until_ms: NOW + 5 * 3_600_000,
        };
        assert_eq!(
            cells(&schedule_view(wait, Some(failed), false)),
            ["next in 5h00m", "failed (exit 2) 1h00m ago"]
        );
        let queued = Decision::Running {
            job: 41,
            status: JobStatus::Queued,
        };
        assert_eq!(
            cells(&schedule_view(queued, Some(ok.clone()), false)),
            ["job 41 queued", "succeeded 1m ago"]
        );
        assert_eq!(
            cells(&schedule_view(queued, None, true)),
            ["paused, job 41 still queued", "-"]
        );
        assert_eq!(
            cells(&schedule_view(Decision::Paused, Some(ok), true)),
            ["paused", "succeeded 1m ago"]
        );
        assert_eq!(
            cells(&schedule_view(Decision::Fire, None, false)),
            ["due now", "-"]
        );
    }

    #[test]
    fn guard_summary_names_the_reason_at_the_action_level() {
        let g = GuardDecision {
            action: GuardAction::Kill,
            reasons: vec![
                "pause: memory pressure Warn".into(),
                "kill: disk free 9347.8 MiB < 10240 MiB".into(),
                "pause: on battery".into(),
            ],
            ..GuardDecision::default()
        };
        assert_eq!(guard_summary(&g), "kill: disk free 9347.8 MiB < 10240 MiB");
    }

    #[test]
    fn account_line_says_whether_idlemax_may_spend_how_much_and_until_when() {
        const NOW: UnixMs = 1_800_000_000_000;
        let account =
            |name: &str, spendable: bool, paused, why: Option<&str>, resets: Option<i64>| {
                AccountCapacity {
                    account: name.into(),
                    headroom_pct: spendable.then_some(54.0),
                    spendable,
                    paused,
                    window: Some(QuotaWindowKind::Weekly),
                    resets_at_ms: resets.map(|ms| NOW + ms),
                    why: why.map(String::from),
                }
            };
        let line = |a| account_line(&a, NOW);
        assert_eq!(
            line(account("claude", true, false, None, Some(52 * 3_600_000))),
            "claude: spendable, 54% left (week), resets in 2d4h"
        );
        assert_eq!(
            line(account(
                "codex",
                false,
                false,
                Some("no usage data yet"),
                None
            )),
            "codex: not spendable now (no usage data yet), quota left unknown (week)"
        );
        assert_eq!(
            line(account("claude", true, true, None, Some(95 * 60_000))),
            "claude: paused by you, 54% left (week), resets in 1h35m"
        );
    }
}
