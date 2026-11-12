//! Delivery: the dispatcher block of `idlemax status` (D17): the line state per device, the
//! workers with the labels of `run_label`, and the two counts, as a table and as TOON.

use idlemax_core::dispatch::view::{DispatcherView, WorkerLine};
use idlemax_core::protocol::ClusterStatus;
use idlemax_core::types::UnixMs;
use idlemax_core::worker::run::RunState;
use serde::Serialize;

use super::toon::Node;
use crate::cli::print::table;
use crate::cli::rows::rel;

pub const WORKER_HEADER: [&str; 6] = ["STATE", "PROJECT", "TASK", "MODEL", "AGE", "SUMMARY"];

/// What `idlemax status --json` prints: the status fields, plus `dispatcher` from a
/// coordinator that serves the view.
#[derive(Serialize)]
pub struct StatusOut<'a> {
    #[serde(flatten)]
    pub status: &'a ClusterStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatcher: Option<&'a DispatcherView>,
}

/// `dispatcher: propose | mac: hold: memory 91% over 90%`.
pub fn line(v: &DispatcherView) -> String {
    let mut parts = vec![format!("dispatcher: {}", v.mode)];
    parts.extend(
        v.devices
            .iter()
            .map(|d| format!("{}: {}: {}", d.id, d.state.as_str(), d.reason)),
    );
    parts.join(" | ")
}

pub fn block(v: &DispatcherView, now: UnixMs) -> String {
    let workers = if v.workers.is_empty() {
        "no workers".to_string()
    } else {
        let rows: Vec<Vec<String>> = v.workers.iter().map(|w| cells(w, now)).collect();
        table(&WORKER_HEADER, &rows)
    };
    format!(
        "{}\n{workers}\nneeds you: {}\nwaiting for review: {}",
        line(v),
        v.needs_you.len(),
        waiting(v)
    )
}

/// Runs in `clean` and `needs_you`: the ones the owner is asked to review, and the two
/// states the review cap counts. A rejected run keeps its branch but waits for nobody.
fn waiting(v: &DispatcherView) -> u64 {
    let n = v
        .review
        .iter()
        .filter(|w| matches!(w.state, RunState::Clean | RunState::NeedsYou))
        .count();
    n as u64
}

fn cells(w: &WorkerLine, now: UnixMs) -> Vec<String> {
    vec![
        w.label.clone(),
        w.project.clone(),
        w.task.clone(),
        w.model.clone(),
        rel(w.start_ms, now),
        w.summary.clone(),
    ]
}

pub fn node(v: &DispatcherView, now: UnixMs) -> Node {
    let devices = v
        .devices
        .iter()
        .map(|d| {
            Node::obj([
                ("id", Node::str(&d.id)),
                ("state", Node::str(d.state.as_str())),
                ("reason", Node::str(&d.reason)),
            ])
        })
        .collect();
    let workers = v
        .workers
        .iter()
        .map(|w| {
            let keys = WORKER_HEADER.map(str::to_lowercase);
            Node::Obj(
                keys.into_iter()
                    .zip(cells(w, now).into_iter().map(Node::Str))
                    .collect(),
            )
        })
        .collect();
    Node::obj([
        ("mode", Node::str(v.mode.as_str())),
        ("devices", Node::Arr(devices)),
        ("workers", Node::Arr(workers)),
        ("needs_you", Node::from(v.needs_you.len() as u64)),
        ("waiting_for_review", Node::from(waiting(v))),
    ])
}
