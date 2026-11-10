//! Delivery: the text and TOON shapes of `idlemax report`. Every number comes from the
//! coordinator's report; this only lays it out.

use std::collections::BTreeMap;

use idlemax_core::report::{duration, QuotaCycle, QuotaSummary, Report};
use idlemax_core::types::UnixMs;

use crate::cli::out::toon::Node;
use crate::cli::rows::{clock, Zone};

const MINUTE_MS: i64 = 60_000;

/// Whole minutes, rounded the way the verdict line rounds them.
fn round_min(ms: i64) -> i64 {
    (ms.max(0) + MINUTE_MS / 2) / MINUTE_MS
}

/// One block per report: the verdict line, then the numbers behind it.
pub(super) fn text(reports: &[Report], zone: Zone) -> String {
    let blocks: Vec<String> = reports.iter().map(|r| block(r, zone)).collect();
    blocks.join("\n\n")
}

/// "14", or "<1" for a job that ran for seconds.
fn job_min(ms: i64) -> String {
    match round_min(ms) {
        0 if ms > 0 => "<1".into(),
        n => n.to_string(),
    }
}

fn block(r: &Report, zone: Zone) -> String {
    let mut lines = vec![r.verdict.clone()];
    let mut row = |name: &str, value: String| lines.push(format!("  {name:<8}{value}"));
    let j = &r.jobs;
    row(
        "jobs",
        format!(
            "{} ran: {} succeeded, {} failed, {} killed, {} cancelled; {} job-min",
            j.ran,
            j.succeeded,
            j.failed,
            j.killed,
            j.cancelled,
            job_min(j.job_ms)
        ),
    );
    if r.guard.tracked_ms > 0 {
        row("held", held_text(r));
        if r.guard.offline_ms > 0 {
            row(
                "away",
                format!("asleep or offline {}", duration(r.guard.offline_ms)),
            );
        }
        row("idle", idle_text(r));
        if !r.idle.stuck_jobs.is_empty() {
            row("stuck", stuck_text(&r.idle.stuck_jobs));
        }
        row("longest", longest_text(r, zone));
    }
    row("quota", quota_text(r));
    lines.join("\n")
}

fn held_text(r: &Report) -> String {
    let g = &r.guard;
    if g.held_ms == 0 {
        return format!(
            "the guard never held work in {} tracked",
            duration(g.tracked_ms)
        );
    }
    let top: Vec<String> = g
        .held_by
        .iter()
        .take(4)
        .map(|h| format!("{} {}", h.label, duration(h.ms)))
        .collect();
    format!(
        "{} of {} tracked: {}",
        duration(g.held_ms),
        duration(g.tracked_ms),
        top.join(", ")
    )
}

fn idle_text(r: &Report) -> String {
    let i = &r.idle;
    let mut text = format!(
        "jobs running {}, queue empty {}, work waiting {}",
        duration(i.used_ms),
        duration(i.queue_empty_ms),
        duration(i.work_waiting_ms)
    );
    if i.quota_waiting_ms > 0 {
        text.push_str(&format!(
            ", AI work waiting for quota {}",
            duration(i.quota_waiting_ms)
        ));
    }
    text
}

/// "5, 9": queued jobs that no device can run, and what to do about it.
fn stuck_text(ids: &[i64]) -> String {
    let list: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!(
        "queued, and no device can run: {}; `idlemax why <id>` says why",
        list.join(", ")
    )
}

/// "7.1 h idle from 11-05 09:04 to 11-05 16:10: held by memory pressure".
fn longest_text(r: &Report, zone: Zone) -> String {
    match &r.idle.longest {
        None => "no idle stretch".into(),
        Some(s) => format!(
            "{} from {} to {}: {}",
            duration(s.end_ms - s.start_ms),
            when(s.start_ms, zone),
            when(s.end_ms, zone),
            s.label
        ),
    }
}

/// "11-05 09:04": the clock without seconds.
fn when(at_ms: UnixMs, zone: Zone) -> String {
    clock(at_ms, zone).chars().take(11).collect()
}

fn quota_text(r: &Report) -> String {
    let q = &r.quota;
    if !q.configured {
        return "no AI accounts are configured".into();
    }
    let lines = quota_lines(q);
    if lines.is_empty() {
        return "no quota window has reset in this period".into();
    }
    let mut text = lines.join("; ");
    if q.counted_since_ms > r.since_ms {
        let kept = duration(r.until_ms - q.counted_since_ms);
        text.push_str(&format!(" (only the last {kept} of quota history is kept)"));
    }
    text
}

/// "claude 5h: 3 expired, 41% unused on average, 1 open at 62% used", per account window.
fn quota_lines(q: &QuotaSummary) -> Vec<String> {
    type Group<'a> = (Vec<&'a QuotaCycle>, Vec<&'a QuotaCycle>);
    let mut groups: BTreeMap<(&str, &str), Group<'_>> = BTreeMap::new();
    for c in &q.expired {
        groups
            .entry((&c.account, &c.window_label))
            .or_default()
            .0
            .push(c);
    }
    for c in &q.open {
        groups
            .entry((&c.account, &c.window_label))
            .or_default()
            .1
            .push(c);
    }
    groups
        .into_iter()
        .map(|((account, window), (expired, open))| {
            let mut parts = Vec::new();
            if !expired.is_empty() {
                let unused: f64 = expired.iter().map(|c| c.unused_pct).sum();
                parts.push(format!(
                    "{} expired, {:.0}% unused on average",
                    expired.len(),
                    unused / expired.len() as f64
                ));
            }
            if let Some(c) = open.last() {
                parts.push(format!("open at {:.0}% used", c.peak_used_pct));
            }
            format!("{account} {window}: {}", parts.join(", "))
        })
        .collect()
}

pub(super) fn view(reports: &[Report], zone: Zone) -> Node {
    let items = reports.iter().map(|r| one(r, zone)).collect();
    Node::obj([("reports", Node::Arr(items))])
}

/// Whole seconds: exact for short periods, and an agent needs no unit conversion.
fn seconds(ms: i64) -> Node {
    Node::Uint(u64::try_from((ms.max(0) + 500) / 1000).unwrap_or(0))
}

fn one(r: &Report, zone: Zone) -> Node {
    let j = &r.jobs;
    let mut fields = vec![
        ("period", Node::str(&r.label)),
        ("verdict", Node::str(&r.verdict)),
        (
            "jobs",
            Node::obj([
                ("ran", Node::Uint(j.ran)),
                ("succeeded", Node::Uint(j.succeeded)),
                ("failed", Node::Uint(j.failed)),
                ("killed", Node::Uint(j.killed)),
                ("cancelled", Node::Uint(j.cancelled)),
                ("job_s", seconds(j.job_ms)),
            ]),
        ),
    ];
    if r.guard.tracked_ms > 0 {
        fields.push(("guard", guard_node(r)));
        fields.push(("idle", idle_node(r, zone)));
    }
    fields.push(("quota", Node::str(quota_text(r))));
    Node::obj(fields)
}

fn guard_node(r: &Report) -> Node {
    let g = &r.guard;
    let held_by = g.held_by.iter().map(|h| {
        Node::obj([
            ("reason", Node::str(h.reason.as_str())),
            ("s", seconds(h.ms)),
        ])
    });
    Node::obj([
        ("tracked_s", seconds(g.tracked_ms)),
        ("held_s", seconds(g.held_ms)),
        ("held_by", Node::Arr(held_by.collect())),
        ("asleep_or_offline_s", seconds(g.offline_ms)),
    ])
}

fn idle_node(r: &Report, zone: Zone) -> Node {
    let i = &r.idle;
    let mut fields = vec![
        ("jobs_running_s", seconds(i.used_ms)),
        ("queue_empty_s", seconds(i.queue_empty_ms)),
        ("work_waiting_s", seconds(i.work_waiting_ms)),
        ("quota_waiting_s", seconds(i.quota_waiting_ms)),
        ("longest", Node::str(longest_text(r, zone))),
    ];
    if !i.stuck_jobs.is_empty() {
        let ids = i.stuck_jobs.iter().map(|id| Node::Int(*id));
        fields.push(("stuck_jobs", Node::Arr(ids.collect())));
    }
    Node::obj(fields)
}
