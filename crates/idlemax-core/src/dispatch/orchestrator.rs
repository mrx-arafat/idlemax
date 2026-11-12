//! Domain: the orchestrator pass (D2): when one is due (A20), what decides that its inputs
//! changed, its output schema, and the strict check of its answer against what the rules
//! offered. Pure. The orchestrator only chooses among offered tasks, accounts and models; a
//! pick outside that set is dropped.

use serde::Deserialize;
use serde_json::{json, Value};

use super::accounts::AccountUse;
use super::plan::{PassPick, PassResult};
use super::project::relative_path_problem;
use super::room::LineState;
use super::task::Task;
use super::{fnv64, DispatchError, Mode};
use crate::control::may_assign;
use crate::protocol::DeviceView;
use crate::quota::Provider;
use crate::types::{is_dispatcher_job, ControlScope, DeviceId, JobSpec, UnixMs};

#[cfg(test)]
mod tests;

pub const PASS_JOB_NAME: &str = "dispatch-orchestrator-pass";
/// The orchestrator is Opus, always (D2, A21).
pub const PASS_MODEL: &str = "opus";
pub const PASS_MIN_GAP_MS: i64 = 3_600_000;
const MAX_PICKS: usize = 8;
const MAX_NEEDS_YOU: usize = 20;
const MAX_REASON_CHARS: usize = 300;
const MAX_SCOPE_PATHS: usize = 20;
const MAX_SCOPE_CHARS: usize = 300;

/// What one pass may choose from.
pub struct Offer<'a> {
    pub tasks: &'a [&'a Task],
    /// The usable accounts.
    pub accounts: &'a [AccountUse],
    pub models: &'a [(Provider, String)],
}

/// What decides whether the pass's inputs changed (A20).
pub struct FingerprintParts<'a> {
    pub devices: &'a [DeviceId],
    pub tasks: &'a [(String, String)],
    pub accounts: &'a [String],
    pub models: &'a [(Provider, String)],
    pub rules: &'a str,
}

pub struct DueInput<'a> {
    pub now_ms: UnixMs,
    pub mode: Mode,
    pub asked: bool,
    pub devices: &'a [DeviceView],
    pub paused: &'a [ControlScope],
    pub offer: &'a Offer<'a>,
    pub fingerprint: &'a str,
    /// (fingerprint, queued_ms) of the newest pass row.
    pub newest: Option<(&'a str, UnixMs)>,
    /// A pass job is queued, leased, running or paused.
    pub pass_active: bool,
}

/// The account and device a due pass runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuePass {
    pub account: String,
    pub device: DeviceId,
}

/// Why `pass_due` refuses because a pass job is already active.
pub const PASS_ACTIVE: &str = "an orchestrator pass is already queued or running";

pub fn is_pass(spec: &JobSpec) -> bool {
    spec.name == PASS_JOB_NAME && is_dispatcher_job(spec)
}

/// `fnv64` over the parts, each list sorted, so order never looks like a change.
pub fn fingerprint(parts: &FingerprintParts<'_>) -> String {
    let mut devices: Vec<&str> = parts.devices.iter().map(String::as_str).collect();
    devices.sort_unstable();
    let mut tasks: Vec<String> = parts
        .tasks
        .iter()
        .map(|(i, d)| format!("{i}={d}"))
        .collect();
    tasks.sort_unstable();
    let mut accounts: Vec<&str> = parts.accounts.iter().map(String::as_str).collect();
    accounts.sort_unstable();
    let mut models: Vec<String> = parts
        .models
        .iter()
        .map(|(p, m)| format!("{p:?}:{m}"))
        .collect();
    models.sort_unstable();
    let joined = [
        devices.join(","),
        tasks.join(","),
        accounts.join(","),
        models.join(","),
    ];
    let mut all: Vec<&str> = joined.iter().map(String::as_str).collect();
    all.push(parts.rules);
    fnv64(&all)
}

/// Devices that take picks now: online, not held, at `run` with an open slot.
pub fn taking_devices<'a>(
    devices: &'a [DeviceView],
    paused: &[ControlScope],
) -> Vec<&'a DeviceView> {
    devices
        .iter()
        .filter(|d| {
            d.online
                && may_assign(paused, &d.info.id, d.local_paused)
                && d.room
                    .as_ref()
                    .is_some_and(|r| r.state == LineState::Run && r.open_slots > 0)
        })
        .collect()
}

/// The account and device to use, or the reason no pass is due.
pub fn pass_due(input: &DueInput<'_>) -> Result<DuePass, String> {
    if input.mode == Mode::Off {
        return Err("the dispatcher is off".into());
    }
    let ready: Vec<&DeviceView> = taking_devices(input.devices, input.paused)
        .into_iter()
        .filter(|d| input.offer.tasks.iter().any(|t| t.device == d.info.id))
        .collect();
    if ready.is_empty() {
        return Err("no device has an open worker slot and a ready task".into());
    }
    let claude = input
        .offer
        .accounts
        .iter()
        .filter(|a| a.provider == Provider::Claude && a.usable);
    let due = claude
        .flat_map(|a| ready.iter().map(move |d| (a, d)))
        .find(|(a, d)| {
            d.info.ai_accounts.contains(&a.account)
                && d.info.capabilities.iter().any(|c| c == "claude")
        });
    let Some((account, device)) = due else {
        return Err("no Claude account may be spent on a device with an open slot".into());
    };
    if input.pass_active {
        return Err(PASS_ACTIVE.into());
    }
    if let (false, Some((print, queued_ms))) = (input.asked, input.newest) {
        if print == input.fingerprint {
            return Err("nothing changed since the last pass".into());
        }
        if input.now_ms - queued_ms < PASS_MIN_GAP_MS {
            return Err("the last pass ran less than an hour ago".into());
        }
    }
    Ok(DuePass {
        account: account.account.clone(),
        device: device.info.id.clone(),
    })
}

/// The JSON schema `claude -p --json-schema` enforces on the pass's answer.
pub fn schema() -> Value {
    let text = |max: usize| json!({"type": "string", "maxLength": max});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["picks", "needs_you"],
        "properties": {
            "picks": {"type": "array", "maxItems": MAX_PICKS, "items": {
                "type": "object", "additionalProperties": false,
                "required": ["task", "account", "model", "reason", "scope_paths"],
                "properties": {
                    "task": {"type": "string"}, "account": {"type": "string"},
                    "model": {"type": "string"}, "reason": text(MAX_REASON_CHARS),
                    "scope_paths": {"type": "array", "maxItems": MAX_SCOPE_PATHS,
                                    "items": text(MAX_SCOPE_CHARS)}
                }
            }},
            "needs_you": {"type": "array", "maxItems": MAX_NEEDS_YOU, "items": {
                "type": "object", "additionalProperties": false,
                "required": ["task", "reason"],
                "properties": {"task": {"type": "string"}, "reason": text(MAX_REASON_CHARS)}
            }}
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    picks: Vec<PassPick>,
    needs_you: Vec<OutNeedsYou>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutNeedsYou {
    task: String,
    reason: String,
}

/// Strict check of the pass's answer. A shape error, an unknown field or a length over a
/// limit rejects the whole pass; a pick outside the offer is dropped and named.
pub fn validate(output: &Value, offer: &Offer<'_>) -> Result<PassResult, DispatchError> {
    let out = Output::deserialize(output).map_err(|e| DispatchError::Invalid(e.to_string()))?;
    check_lengths(&out).map_err(DispatchError::Invalid)?;
    let mut result = PassResult::default();
    for n in out.needs_you {
        if offer.tasks.iter().any(|t| t.id == n.task) {
            result.needs_you.push((n.task, n.reason));
        } else {
            result
                .dropped
                .push(format!("needs_you {}: not an offered task", n.task));
        }
    }
    for p in out.picks {
        match pick_problem(&p, offer, &result) {
            Some(why) => result.dropped.push(format!("{}: {why}", p.task)),
            None => result.picks.push(p),
        }
    }
    Ok(result)
}

fn check_lengths(out: &Output) -> Result<(), String> {
    let long = |s: &str| s.chars().count() > MAX_REASON_CHARS;
    if out.picks.len() > MAX_PICKS {
        return Err(format!("more than {MAX_PICKS} picks"));
    }
    if out.needs_you.len() > MAX_NEEDS_YOU {
        return Err(format!("more than {MAX_NEEDS_YOU} needs_you entries"));
    }
    if out.picks.iter().any(|p| long(&p.reason)) || out.needs_you.iter().any(|n| long(&n.reason)) {
        return Err(format!("a reason is over {MAX_REASON_CHARS} characters"));
    }
    let bad_scope = out.picks.iter().any(|p| {
        p.scope_paths.len() > MAX_SCOPE_PATHS
            || p.scope_paths
                .iter()
                .any(|s| s.chars().count() > MAX_SCOPE_CHARS)
    });
    if bad_scope {
        return Err(format!(
            "a pick has more than {MAX_SCOPE_PATHS} scope paths or one over {MAX_SCOPE_CHARS} characters"
        ));
    }
    Ok(())
}

/// Why one pick cannot stand, or None.
fn pick_problem(p: &PassPick, offer: &Offer<'_>, so_far: &PassResult) -> Option<String> {
    if !offer.tasks.iter().any(|t| t.id == p.task) {
        return Some("not an offered task".into());
    }
    if so_far.needs_you.iter().any(|(t, _)| *t == p.task) {
        return Some("also sent to needs_you".into());
    }
    if so_far.picks.iter().any(|q| q.task == p.task) {
        return Some("picked twice".into());
    }
    let Some(account) = offer.accounts.iter().find(|a| a.account == p.account) else {
        return Some(format!("account {} was not offered", p.account));
    };
    if !offer.models.contains(&(account.provider, p.model.clone())) {
        return Some(format!(
            "model {} was not offered for {}",
            p.model, p.account
        ));
    }
    p.scope_paths
        .iter()
        .find_map(|s| relative_path_problem(s).err())
        .map(|e| format!("scope path: {e}"))
}
