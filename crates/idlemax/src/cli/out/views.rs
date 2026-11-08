//! Delivery: the small, decision-ready TOON shape of each response (AXI minimal schemas).
//! The raw API value stays available as `--json`.

use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::schedule::ScheduleView;
use idlemax_core::types::{ControlAction, Job, Recommendation};

use super::toon::Node;
use crate::cli::rows::snake;

/// A reply to a mutation: what happened, then the thing it happened to.
fn reply(result: &str, subject: Option<(&str, Node)>) -> Node {
    let mut fields = vec![("result".to_string(), Node::str(result))];
    fields.extend(subject.map(|(k, v)| (k.to_string(), v)));
    Node::Obj(fields)
}

/// "paused global", "resumed device mac": what a control request did.
pub fn done(req: &ControlRequest) -> String {
    let verb = match req.action {
        ControlAction::Pause => "paused",
        ControlAction::Resume => "resumed",
        ControlAction::StopAll => "stopped",
        ControlAction::Cancel => "cancelled",
    };
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

pub fn schedule_result(result: &str, v: &ScheduleView) -> Node {
    let s = &v.schedule;
    let schedule = Node::obj([
        ("id", Node::Int(s.id)),
        ("name", Node::str(&s.spec.name)),
        ("every", Node::str(s.every.to_string())),
        ("paused", Node::Bool(s.paused)),
    ]);
    reply(result, Some(("schedule", schedule)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::out::toon::encode;
    use idlemax_core::protocol::DeviceView;
    use idlemax_core::types::ControlScope;

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
