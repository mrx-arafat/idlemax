//! Delivery: the small, decision-ready TOON shape of each response (AXI minimal schemas).
//! The raw API value stays available as `--json`.

use idlemax_core::protocol::{ClusterStatus, ControlRequest, JobCounts, LocalStatus};
use idlemax_core::schedule::ScheduleView;
use idlemax_core::types::{ControlAction, Job, JobStatus, Recommendation, UnixMs};

use super::failure::CliError;
use super::lists;
use super::toon::Node;
use crate::cli::rows::{guard_summary, rel, snake};

/// A reply to a mutation: what happened, then the thing it happened to.
fn reply(result: &str, subject: Option<(&str, Node)>) -> Node {
    let mut fields = vec![("result".to_string(), Node::str(result))];
    fields.extend(subject.map(|(k, v)| (k.to_string(), v)));
    Node::Obj(fields)
}

/// Past tense of a control action.
pub fn verb(action: ControlAction) -> &'static str {
    match action {
        ControlAction::Pause => "paused",
        ControlAction::Resume => "resumed",
        ControlAction::StopAll => "stopped",
        ControlAction::Cancel => "cancelled",
    }
}

/// "paused global", "resumed device mac": what a control request did.
pub fn done(req: &ControlRequest) -> String {
    let verb = verb(req.action);
    let label = crate::cli::print::control_label(req);
    let scope = label.split_once(' ').map_or("", |(_, scope)| scope);
    format!("{verb} {scope}")
}

/// The cluster in a few lines: pause state, job counts, open advice.
pub fn summary(s: &ClusterStatus) -> Vec<(String, Node)> {
    let j = &s.jobs;
    let mut counts = vec![("running", j.running + j.leased), ("queued", j.queued)];
    counts.extend(
        [
            ("paused", j.paused),
            ("parked", j.parked),
            ("failed", j.failed),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0),
    );
    let paused_devices: Vec<&str> = s
        .devices
        .iter()
        .filter(|d| d.paused)
        .map(|d| d.info.id.as_str())
        .collect();
    let mut fields = vec![
        ("coordinator".to_string(), Node::str(&s.version)),
        (
            "global".to_string(),
            Node::str(if s.global_paused { "paused" } else { "running" }),
        ),
    ];
    for (key, names) in [
        (
            "paused_accounts",
            s.paused_accounts.iter().map(String::as_str).collect(),
        ),
        ("paused_devices", paused_devices),
    ] {
        if !names.is_empty() {
            fields.push((
                key.to_string(),
                Node::Arr(names.into_iter().map(Node::str).collect()),
            ));
        }
    }
    fields.push((
        "jobs".to_string(),
        Node::obj(counts.into_iter().map(|(k, n)| (k, Node::Uint(n)))),
    ));
    fields.push(("advice_open".to_string(), Node::Uint(s.open_advice)));
    fields
}

pub fn control(result: &str, s: &ClusterStatus) -> Node {
    let mut fields = vec![("result".to_string(), Node::str(result))];
    fields.extend(summary(s));
    Node::Obj(fields)
}

pub fn job_result(result: &str, j: &Job) -> Node {
    let mut job = vec![
        ("id", Node::Int(j.id)),
        ("name", Node::str(&j.spec.name)),
        ("status", Node::str(j.status.to_string())),
    ];
    if let Some(d) = &j.device {
        job.push(("device", Node::str(d)));
    }
    reply(result, Some(("job", Node::obj(job))))
}

pub fn advice_result(result: &str, r: &Recommendation) -> Node {
    let advice = Node::obj([
        ("id", Node::Int(r.id)),
        ("title", Node::str(&r.title)),
        ("status", Node::str(snake(&r.status))),
    ]);
    reply(result, Some(("advice", advice)))
}

pub fn schedule_result(result: &str, v: &ScheduleView, now: UnixMs, note: Option<String>) -> Node {
    let s = &v.schedule;
    let schedule = Node::obj([
        ("id", Node::Int(s.id)),
        ("name", Node::str(&s.spec.name)),
        ("every", Node::str(s.every.to_string())),
        ("state", Node::str(crate::cli::rows::schedule_state(v, now))),
    ]);
    let mut fields = vec![
        ("result".to_string(), Node::str(result)),
        ("schedule".to_string(), schedule),
    ];
    fields.extend(note.map(|n| ("note".to_string(), Node::Str(n))));
    Node::Obj(fields)
}

/// How many jobs exist under a status filter, from the cluster's counts.
pub fn job_total(c: &JobCounts, filter: Option<JobStatus>) -> u64 {
    match filter {
        None => {
            c.queued
                + c.leased
                + c.running
                + c.paused
                + c.succeeded
                + c.failed
                + c.killed
                + c.cancelled
                + c.parked
        }
        Some(JobStatus::Queued) => c.queued,
        Some(JobStatus::Leased) => c.leased,
        Some(JobStatus::Running) => c.running,
        Some(JobStatus::Paused) => c.paused,
        Some(JobStatus::Succeeded) => c.succeeded,
        Some(JobStatus::Failed) => c.failed,
        Some(JobStatus::Killed) => c.killed,
        Some(JobStatus::Cancelled) => c.cancelled,
        Some(JobStatus::Parked) => c.parked,
    }
}

/// `idlemax status`: the summary, then the devices.
pub fn status(s: &ClusterStatus, now: UnixMs, fields: &[String]) -> Result<Node, CliError> {
    let mut out = summary(s);
    if let Node::Obj(devices) = lists::devices(s, now, fields)? {
        out.extend(devices.into_iter().filter(|(k, _)| k != "count"));
    }
    Ok(Node::Obj(out))
}

/// A queued job, with the warning that no device would run it when there is one.
pub fn job_added(result: &str, j: &Job, warning: Option<&str>) -> Node {
    let Node::Obj(mut fields) = job_result(result, j) else {
        return Node::Null;
    };
    fields.extend(warning.map(|w| ("warning".to_string(), Node::str(w))));
    Node::Obj(fields)
}

/// `idlemax local ...`: this device's agent in a few lines.
pub fn local(result: Option<&str>, s: &LocalStatus, now: UnixMs) -> Node {
    let coordinator = match (s.coordinator_reachable, s.last_heartbeat_ms) {
        (true, _) => "reachable".to_string(),
        (false, Some(t)) => format!("unreachable (last heartbeat {})", rel(t, now)),
        (false, None) => "unreachable (no heartbeat yet)".to_string(),
    };
    let mut fields = vec![];
    fields.extend(result.map(|r| ("result".to_string(), Node::str(r))));
    fields.extend([
        ("device".to_string(), Node::str(&s.device)),
        ("agent".to_string(), Node::str(&s.version)),
        ("coordinator".to_string(), Node::Str(coordinator)),
        ("local_pause".to_string(), Node::Bool(s.local_paused)),
        ("kill_switch".to_string(), Node::Bool(s.kill_switch)),
        ("guard".to_string(), Node::Str(guard_summary(&s.guard))),
    ]);
    if s.running.is_empty() {
        fields.push(("running".to_string(), Node::str("0 jobs")));
    } else {
        let rows = s.running.iter().map(|j| {
            Node::obj([
                ("id", Node::Int(j.id)),
                ("status", Node::str(j.status.to_string())),
                ("name", Node::str(&j.name)),
                ("since", Node::str(rel(j.started_ms, now))),
            ])
        });
        fields.push(("running".to_string(), Node::Arr(rows.collect())));
    }
    Node::Obj(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::out::toon::encode;
    use idlemax_core::protocol::DeviceView;
    use idlemax_core::types::ControlScope;

    #[test]
    fn the_job_total_follows_the_status_filter() {
        let c = JobCounts {
            queued: 3,
            running: 1,
            failed: 2,
            ..JobCounts::default()
        };
        assert_eq!(job_total(&c, None), 6);
        assert_eq!(job_total(&c, Some(JobStatus::Failed)), 2);
        assert_eq!(job_total(&c, Some(JobStatus::Parked)), 0);
    }

    #[test]
    fn a_control_reply_is_the_result_then_a_short_summary() {
        let mut mac = DeviceView::default();
        mac.info.id = "mac".into();
        mac.paused = true;
        let mut s = ClusterStatus {
            version: "0.1.0".into(),
            global_paused: true,
            paused_accounts: vec!["claude".into()],
            devices: vec![mac],
            open_advice: 2,
            ..ClusterStatus::default()
        };
        s.jobs.queued = 3;
        s.jobs.failed = 1;
        s.jobs.leased = 1;
        let req = ControlRequest {
            scope: ControlScope::Device("mac".into()),
            action: ControlAction::Pause,
            reason: None,
        };
        assert_eq!(done(&req), "paused device mac");
        assert_eq!(
            encode(&control(&done(&req), &s)),
            "result: paused device mac\ncoordinator: 0.1.0\nglobal: paused\n\
             paused_accounts[1]: claude\npaused_devices[1]: mac\n\
             jobs:\n  running: 1\n  queued: 3\n  failed: 1\nadvice_open: 2"
        );
    }
}
