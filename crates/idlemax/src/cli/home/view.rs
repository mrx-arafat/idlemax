//! Delivery: the home view and the hook summary as TOON. Token-small on purpose: the hook
//! summary is loaded into every agent session, so it names only what needs a decision.

use super::render::{self, DeviceLine, Home, Live, Mode, QuotaLine, Reason, State};
use crate::cli::out::toon::Node;
use crate::cli::out::views::job_counts;
use crate::cli::rows::snake;

/// The TOON document and its next-step hints.
pub fn of(h: &Home, mode: Mode) -> (Node, Vec<String>) {
    match (&h.state, mode) {
        (State::Up(live), Mode::Full) => {
            let mut fields = head(h);
            fields.extend(live_fields(live));
            (Node::Obj(fields), render::up_steps(live))
        }
        (State::Up(live), Mode::Hook) => {
            let mut fields = live_fields(live);
            let help = match live.review {
                0 => "Run `idlemax` for the full view".to_string(),
                n => format!(
                    "{}; run `idlemax` for the full view",
                    render::review_note(n)
                ),
            };
            fields.push(("help".into(), Node::Str(help)));
            (Node::Obj(fields), Vec::new())
        }
        (State::Down { reason, detail }, Mode::Full) => {
            let mut fields = head(h);
            fields.push(("status".into(), Node::str(snake(reason))));
            if *reason == Reason::Failed {
                fields.push(("detail".into(), Node::str(detail)));
            }
            (Node::Obj(fields), render::down_steps(*reason))
        }
        (State::Down { reason, detail }, Mode::Hook) => {
            let fix = if *reason == Reason::NotSetUp {
                "setup"
            } else {
                "doctor"
            };
            let note = format!(
                "{} (run `idlemax {fix}`)",
                render::down_line(h, *reason, detail)
            );
            let note = note.strip_prefix("idlemax: ").unwrap_or(&note).to_string();
            (Node::obj([("idlemax", Node::Str(note))]), Vec::new())
        }
    }
}

fn head(h: &Home) -> Vec<(String, Node)> {
    vec![
        ("bin".into(), Node::str(&h.bin)),
        ("description".into(), Node::str(h.description)),
        ("coordinator".into(), Node::str(&h.coordinator)),
    ]
}

fn live_fields(live: &Live) -> Vec<(String, Node)> {
    let mut fields = vec![
        ("devices".into(), devices(&live.devices)),
        ("jobs".into(), job_counts(&live.jobs)),
    ];
    if live.open_advice > 0 {
        fields.push(("advice_open".into(), Node::Uint(live.open_advice)));
    }
    if live.review > 0 {
        fields.push(("review_waiting".into(), Node::Uint(live.review)));
    }
    if !live.quota.is_empty() {
        fields.push(("quota".into(), quota(&live.quota)));
    }
    fields
}

fn devices(list: &[DeviceLine]) -> Node {
    if list.is_empty() {
        return Node::str("none have sent a heartbeat yet");
    }
    let rows = list.iter().map(|d| {
        Node::obj([
            ("id", Node::str(&d.id)),
            ("state", Node::str(&d.state)),
            (
                "guard",
                if d.guard == "-" {
                    Node::Null
                } else {
                    Node::str(&d.guard)
                },
            ),
        ])
    });
    Node::Arr(rows.collect())
}

fn quota(list: &[QuotaLine]) -> Node {
    let rows = list.iter().map(|q| {
        Node::obj([
            ("account", Node::str(&q.account)),
            ("spendable", Node::Bool(q.spendable)),
        ])
    });
    Node::Arr(rows.collect())
}
