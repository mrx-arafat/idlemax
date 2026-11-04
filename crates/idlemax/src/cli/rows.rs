//! Delivery: turns API views into display cells, shared by CLI tables and the TUI.

use idlemax_core::protocol::DeviceView;
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{
    Event, GuardAction, GuardDecision, Job, JobStatus, QuotaWindowKind, Recommendation, UnixMs,
};
use serde::Serialize;

pub const DEVICE_HEADER: [&str; 11] = [
    "DEVICE", "STATE", "GUARD", "IDLE", "CPU", "LOAD", "MEM FREE", "SWAP", "DISK", "POWER", "JOBS",
];
pub const JOB_HEADER: [&str; 9] = [
    "ID", "NAME", "STATUS", "DEVICE", "COST", "PRIO", "TRIES", "AGE", "RESULT",
];
pub const QUOTA_HEADER: [&str; 8] = [
    "ACCOUNT", "WINDOW", "USED", "RESERVE", "HEADROOM", "RESETS", "SPEND", "WHY",
];
pub const ADVICE_HEADER: [&str; 7] = ["ID", "KIND", "TITLE", "CONF", "SOURCE", "STATUS", "AGE"];
pub const EVENT_HEADER: [&str; 5] = ["TIME", "KIND", "DEVICE", "JOB", "MESSAGE"];

pub fn device_cells(d: &DeviceView, now_ms: UnixMs) -> Vec<String> {
    let m = d.metrics.as_ref();
    let paused_jobs = d
        .running
        .iter()
        .filter(|j| j.status == JobStatus::Paused)
        .count();
    vec![
        d.info.id.clone(),
        device_state(d, now_ms),
        d.guard.as_ref().map_or_else(|| "-".into(), guard_summary),
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
        match paused_jobs {
            0 => d.running.len().to_string(),
            p => format!("{} ({p} paused)", d.running.len()),
        },
    ]
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
    let Some(reason) = g.reasons.first() else {
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
    if let Some(err) = &j.last_error {
        return trunc(err, 48);
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

pub fn event_cells(e: &Event) -> Vec<String> {
    vec![
        clock(e.at_ms),
        snake(&e.kind),
        e.device.clone().unwrap_or_else(|| "-".into()),
        e.job.map_or_else(|| "-".into(), |j| j.to_string()),
        e.message.clone(),
    ]
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

/// Local wall-clock time, "11-05 03:12:09".
pub fn clock(at_ms: UnixMs) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms).map_or_else(
        || "-".into(),
        |t| {
            t.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M:%S")
                .to_string()
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
