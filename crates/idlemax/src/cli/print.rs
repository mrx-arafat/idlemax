//! Delivery: plain-text rendering of API responses for the CLI.

use idlemax_core::doctor::Report;
use idlemax_core::protocol::{ClusterStatus, ControlRequest, LocalStatus};
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{ControlScope, Event, Job, Recommendation, UnixMs};

use super::rows::{self, guard_summary, rel};

/// Left-aligned columns separated by two spaces.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = &str>| {
        let padded: Vec<String> = cells
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}", w = *w))
            .collect();
        padded.join("  ").trim_end().to_string()
    };
    let mut out = line(&mut header.iter().copied());
    for row in rows {
        out.push('\n');
        out.push_str(&line(&mut row.iter().map(String::as_str)));
    }
    out
}

/// One line: version, pause state, non-zero job counts, open advice.
pub fn summary(s: &ClusterStatus) -> String {
    format!("coordinator {} | {}", s.version, status_line(s))
}

/// `summary` without the version, for screens too narrow to show it.
pub fn status_line(s: &ClusterStatus) -> String {
    let global = if s.global_paused { "PAUSED" } else { "running" };
    let accounts = match s.paused_accounts.as_slice() {
        [] => String::new(),
        list => format!(" | paused accounts: {}", list.join(", ")),
    };
    let j = &s.jobs;
    let mut jobs = vec![format!("{} running", j.running + j.leased)];
    for (n, word) in [
        (j.paused, "paused"),
        (j.queued, "queued"),
        (j.parked, "parked"),
        (j.failed, "failed"),
    ] {
        if n > 0 || word == "queued" {
            jobs.push(format!("{n} {word}"));
        }
    }
    format!(
        "global: {global}{accounts} | jobs: {} | advice: {} open",
        jobs.join(", "),
        s.open_advice
    )
}

/// "pause device mac", "stop_all global".
pub fn control_label(r: &ControlRequest) -> String {
    let scope = match &r.scope {
        ControlScope::Global => "global".to_string(),
        ControlScope::Device(d) => format!("device {d}"),
        ControlScope::Account(a) => format!("account {a}"),
        ControlScope::Job(j) => format!("job {j}"),
    };
    format!("{} {scope}", rows::snake(&r.action))
}

pub fn status(s: &ClusterStatus, now_ms: UnixMs) -> String {
    let rows: Vec<_> = s
        .devices
        .iter()
        .map(|d| rows::device_cells(d, now_ms))
        .collect();
    let devices = if rows.is_empty() {
        "no devices have sent a heartbeat yet".to_string()
    } else {
        table(&rows::DEVICE_HEADER, &rows)
    };
    format!("{}\n\n{devices}", summary(s))
}

pub fn jobs(jobs: &[Job], now_ms: UnixMs) -> String {
    if jobs.is_empty() {
        return "no jobs".into();
    }
    let rows: Vec<_> = jobs.iter().map(|j| rows::job_cells(j, now_ms)).collect();
    table(&rows::JOB_HEADER, &rows)
}

pub fn job_line(j: &Job) -> String {
    format!("job {} {}: {}", j.id, j.status, j.spec.name)
}

pub fn quota(views: &[QuotaView], paused_accounts: &[String], now_ms: UnixMs) -> String {
    if views.is_empty() {
        return "no AI accounts configured ([[quota.accounts]] in config.toml)".into();
    }
    let rows: Vec<_> = views
        .iter()
        .map(|q| rows::quota_cells(q, paused_accounts.contains(&q.account), now_ms))
        .collect();
    table(&rows::QUOTA_HEADER, &rows)
}

pub fn advice(list: &[Recommendation], now_ms: UnixMs) -> String {
    if list.is_empty() {
        return "no advice".into();
    }
    let rows: Vec<_> = list.iter().map(|r| rows::advice_cells(r, now_ms)).collect();
    table(&rows::ADVICE_HEADER, &rows)
}

pub fn advice_line(r: &Recommendation) -> String {
    format!("advice {} {}: {}", r.id, rows::snake(&r.status), r.title)
}

pub fn events(list: &[Event]) -> String {
    if list.is_empty() {
        return "no events".into();
    }
    let rows: Vec<_> = list.iter().map(rows::event_cells).collect();
    table(&rows::EVENT_HEADER, &rows)
}

/// One line per check, an indented fix line under each warn or fail, then a summary.
pub fn doctor(r: &Report) -> String {
    let width = r
        .checks
        .iter()
        .map(|c| c.id.chars().count())
        .max()
        .unwrap_or(0)
        .max("service".len());
    let mut out = format!("idlemax {} doctor\n", r.version);
    for check in &r.checks {
        out.push_str(&format!(
            "{:<4}  {:<width$}  {}\n",
            check.level.word(),
            check.id,
            check.detail,
            width = width
        ));
        if let Some(fix) = &check.fix {
            out.push_str(&format!(
                "      {:<width$}  fix: {}\n",
                "",
                fix,
                width = width
            ));
        }
    }
    out.push_str(&format!(
        "\n{} checks: {} ok, {} warn, {} fail",
        r.checks.len(),
        r.counts.ok,
        r.counts.warn,
        r.counts.fail
    ));
    out.push_str(match r.healthy {
        true => "\nall checks passed",
        false => "\nfix the failing checks above, then run `idlemax doctor` again",
    });
    out
}

pub fn local_status(s: &LocalStatus, now_ms: UnixMs) -> String {
    let coord = match (s.coordinator_reachable, s.last_heartbeat_ms) {
        (true, _) => "reachable".to_string(),
        (false, Some(t)) => format!("UNREACHABLE (last heartbeat {})", rel(t, now_ms)),
        (false, None) => "UNREACHABLE (no heartbeat yet)".to_string(),
    };
    let mut out = format!(
        "device {} agent {}\ncoordinator: {coord}\nlocal pause: {}\nkill switch file: {}\nguard: {}",
        s.device,
        s.version,
        yes_no(s.local_paused),
        yes_no(s.kill_switch),
        guard_summary(&s.guard),
    );
    for reason in s.guard.reasons.iter().skip(1) {
        out.push_str(&format!("\n  {reason}"));
    }
    out.push_str(&format!("\nrunning jobs: {}", s.running.len()));
    for j in &s.running {
        out.push_str(&format!(
            "\n  {} {} {} since {}",
            j.id,
            j.status,
            j.name,
            rel(j.started_ms, now_ms)
        ));
    }
    out
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
