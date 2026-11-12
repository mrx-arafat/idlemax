//! Domain: the Plan: per device the room, the open slots and the picks that would fill them,
//! with a reason for every pick and every empty slot. Pure.
//!
//! The rules decide whether and how many (D2, D13); every pick comes from an orchestrator
//! pass, and a ready task no pass has seen is listed as not judged and fills no slot (A19).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::accounts::AccountUse;
use super::inputs::StoredRepo;
use super::project::ProjectView;
use super::proposal::RuleProposal;
use super::room::{LineState, RoomDecision};
use super::task::Task;
use super::Mode;
use crate::control::may_assign;
use crate::protocol::DeviceView;
use crate::quota::Provider;
use crate::types::{ControlScope, DeviceId, UnixMs};

#[cfg(test)]
mod tests;

/// Who made a pick. The only value: no slot is filled without a pass (A19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickedBy {
    Orchestrator,
}

/// One worker the dispatcher would launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub project: String,
    pub task: String,
    pub title: String,
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub reason: String,
    pub by: PickedBy,
    pub scope_paths: Vec<String>,
    /// Paths a worker must not touch: the project's off-limits paths, plus the owner's
    /// changed files while the owner is active in the project.
    pub avoid_paths: Vec<String>,
    /// In `auto`, why the launch step holds this pick now; absent when it may launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptySlots {
    pub count: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevicePlan {
    pub device: DeviceId,
    pub room: RoomDecision,
    /// Why the device takes nothing whatever its room: offline, paused, kill switch.
    pub held: Option<String>,
    pub picks: Vec<Pick>,
    pub empty: Option<EmptySlots>,
    /// Whether the device is switched on for dispatcher work; absent from a coordinator
    /// that predates the switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub at_ms: UnixMs,
    pub mode: Mode,
    /// The pass the picks come from and its age, or why no pass has run.
    pub picked_by: String,
    pub devices: Vec<DevicePlan>,
    /// Ready tasks no orchestrator pass has seen, in plan order; never a pick (A19).
    pub not_judged: Vec<Task>,
    pub needs_you: Vec<Task>,
    pub accounts: Vec<AccountUse>,
    pub notes: Vec<String>,
}

/// One pick as the orchestrator returned it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PassPick {
    pub task: String,
    pub account: String,
    pub model: String,
    pub reason: String,
    pub scope_paths: Vec<String>,
}

/// A checked orchestrator pass.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PassResult {
    pub picks: Vec<PassPick>,
    /// (task id, reason)
    pub needs_you: Vec<(String, String)>,
    /// Picks the check dropped, each with why.
    pub dropped: Vec<String>,
    /// Ids of the tasks the pass was offered.
    pub seen: Vec<String>,
    /// One line proposed for `rules.md` after a rejected or discarded run (D18.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_proposal: Option<RuleProposal>,
}

pub struct PlanInput<'a> {
    pub now_ms: UnixMs,
    pub mode: Mode,
    pub devices: &'a [DeviceView],
    pub paused: &'a [ControlScope],
    pub projects: &'a [ProjectView],
    pub repos: &'a [StoredRepo],
    pub tasks: &'a [Task],
    pub accounts: &'a [AccountUse],
    pub models: &'a [(Provider, String)],
    pub pass: Option<&'a PassResult>,
    pub picked_by: &'a str,
    pub notes: &'a [String],
    /// Picks the quota rules refused now (task id, the refusal's text); they fill no slot.
    pub refused: &'a [(String, String)],
}

/// The ready tasks of a device's switched-on projects, in plan order: projects by priority
/// (higher first) then name, tasks in the order `discover` found them.
pub fn offered<'a>(input: &PlanInput<'a>, device: &str) -> Vec<&'a Task> {
    let mut projects: Vec<&ProjectView> = input
        .projects
        .iter()
        .filter(|v| v.project.enabled && v.project.device == device)
        .collect();
    projects.sort_by(|a, b| {
        let (a, b) = (&a.project, &b.project);
        b.settings
            .priority
            .cmp(&a.settings.priority)
            .then(a.name.cmp(&b.name))
    });
    projects
        .iter()
        .flat_map(|v| {
            input
                .tasks
                .iter()
                .filter(move |t| t.project_id == v.project.id && t.needs_you.is_none())
        })
        .collect()
}

pub fn build(input: &PlanInput<'_>) -> Plan {
    let mut used = BTreeSet::new();
    let devices = input
        .devices
        .iter()
        .map(|dv| device_plan(input, dv, &mut used))
        .collect();
    let seen = |t: &Task| input.pass.is_some_and(|p| p.seen.contains(&t.id));
    let mut not_judged: Vec<Task> = Vec::new();
    for dv in input.devices {
        for t in offered(input, &dv.info.id) {
            if !seen(t) && !not_judged.iter().any(|n| n.id == t.id) {
                not_judged.push(t.clone());
            }
        }
    }
    Plan {
        at_ms: input.now_ms,
        mode: input.mode,
        picked_by: input.picked_by.to_string(),
        devices,
        not_judged,
        needs_you: input
            .tasks
            .iter()
            .filter(|t| t.needs_you.is_some())
            .cloned()
            .collect(),
        accounts: input.accounts.to_vec(),
        notes: input.notes.to_vec(),
    }
}

/// Why the device takes nothing whatever its room.
fn held(input: &PlanInput<'_>, dv: &DeviceView) -> Option<String> {
    if let Some(gate) = crate::control::device_gate(dv) {
        Some(gate)
    } else if !dv.online {
        Some("offline".into())
    } else if !may_assign(input.paused, &dv.info.id, false) {
        Some("paused by you: idlemax resume".into())
    } else if dv.local_paused {
        Some("kill switch or local pause is on".into())
    } else {
        None
    }
}

fn device_plan(input: &PlanInput<'_>, dv: &DeviceView, used: &mut BTreeSet<String>) -> DevicePlan {
    let room = dv
        .room
        .clone()
        .unwrap_or_else(|| RoomDecision::hold("this agent reports no worker room", input.now_ms));
    let held = held(input, dv);
    let slots = if input.mode != Mode::Off && held.is_none() && room.state == LineState::Run {
        room.open_slots
    } else {
        0
    };
    let offered = offered(input, &dv.info.id);
    let (picks, first_refusal) = fill_slots(input, dv, &offered, slots, used);
    let left = slots.saturating_sub(u32::try_from(picks.len()).unwrap_or(u32::MAX));
    let empty = (left > 0).then(|| EmptySlots {
        count: left,
        reason: empty_reason(input, &offered, used, first_refusal),
    });
    DevicePlan {
        device: dv.info.id.clone(),
        room,
        held,
        picks,
        empty,
        enabled: dv.enabled,
    }
}

/// The pass's picks that fill this device's slots, in the pass's order, and why the first
/// pick that could not was refused.
fn fill_slots(
    input: &PlanInput<'_>,
    dv: &DeviceView,
    offered: &[&Task],
    slots: u32,
    used: &mut BTreeSet<String>,
) -> (Vec<Pick>, Option<String>) {
    let mut picks = Vec::new();
    let mut first_refusal = None;
    for pp in input.pass.map(|p| p.picks.as_slice()).unwrap_or_default() {
        if picks.len() >= slots as usize {
            break;
        }
        let Some(task) = offered
            .iter()
            .find(|t| t.id == pp.task && !used.contains(&t.id))
        else {
            continue;
        };
        if let Some((_, why)) = input.refused.iter().find(|(id, _)| *id == task.id) {
            first_refusal.get_or_insert(format!("{}: {why}", task.title));
            continue;
        }
        match check_pick(input, dv, offered, task, pp) {
            Ok(pick) => {
                used.insert(task.id.clone());
                picks.push(pick);
            }
            Err(why) => {
                first_refusal.get_or_insert(why);
            }
        }
    }
    (picks, first_refusal)
}

/// The reason for the slots left over: the first that applies.
fn empty_reason(
    input: &PlanInput<'_>,
    offered: &[&Task],
    used: &BTreeSet<String>,
    refusal: Option<String>,
) -> String {
    let left: Vec<&&Task> = offered.iter().filter(|t| !used.contains(&t.id)).collect();
    if left.is_empty() {
        return "no ready task".into();
    }
    let unseen = left
        .iter()
        .filter(|t| !input.pass.is_some_and(|p| p.seen.contains(&t.id)))
        .count();
    if unseen > 0 {
        return format!(
            "{unseen} ready tasks are not judged by the orchestrator: {}",
            input.picked_by
        );
    }
    refusal.unwrap_or_else(|| "the orchestrator picked no task for this slot".into())
}

/// A pass pick that the rules allow, or why it cannot fill a slot here.
fn check_pick(
    input: &PlanInput<'_>,
    dv: &DeviceView,
    offered: &[&Task],
    task: &Task,
    pp: &PassPick,
) -> Result<Pick, String> {
    let title = &task.title;
    let account = usable_account(input, title, &pp.account)?;
    let info = &dv.info;
    if !info.ai_accounts.contains(&account.account) {
        return Err(format!(
            "{title}: account {} is not in {}'s agent.ai_accounts",
            account.account, info.id
        ));
    }
    let cli = account.provider.cli().unwrap_or("api");
    if !info.capabilities.iter().any(|c| c == cli) {
        return Err(format!("{title}: {cli} is not installed on {}", info.id));
    }
    if !input.models.contains(&(account.provider, pp.model.clone())) {
        let provider = provider_name(account.provider);
        return Err(format!(
            "{title}: no tier of {provider} uses the model {}",
            pp.model
        ));
    }
    let avoid = owner_rule(input, offered, task)?;
    Ok(Pick {
        project: task.project.clone(),
        task: task.id.clone(),
        title: title.clone(),
        provider: account.provider,
        account: account.account.clone(),
        model: pp.model.clone(),
        reason: pp.reason.clone(),
        by: PickedBy::Orchestrator,
        scope_paths: pp.scope_paths.clone(),
        avoid_paths: avoid,
        held: None,
    })
}

/// The account of a pick, when the simple rule lets it be spent.
fn usable_account<'a>(
    input: &PlanInput<'a>,
    title: &str,
    name: &str,
) -> Result<&'a AccountUse, String> {
    let account = input
        .accounts
        .iter()
        .find(|a| a.account == name)
        .ok_or_else(|| format!("{title}: account {name} is not configured"))?;
    if !account.usable {
        return Err(format!(
            "{title}: account {name} may not be spent: {}",
            account.why
        ));
    }
    Ok(account)
}

/// D5 as a fixed rule: a task in a project the owner is active in is used only when no
/// offered task is in a project the owner is not active in, and never while the list of the
/// owner's changed files is cut. Returns the paths the worker must avoid.
fn owner_rule(
    input: &PlanInput<'_>,
    offered: &[&Task],
    task: &Task,
) -> Result<Vec<String>, String> {
    let view = |id: i64| input.projects.iter().find(|v| v.project.id == id);
    let Some(project) = view(task.project_id) else {
        return Err(format!("{}: its project is gone", task.title));
    };
    let mut avoid = project.project.settings.off_limits.clone();
    if !project.owner_active {
        return Ok(avoid);
    }
    let refused = || format!("{}: the owner is active in {}", task.title, task.project);
    let elsewhere = offered
        .iter()
        .any(|t| view(t.project_id).is_some_and(|v| !v.owner_active));
    let repo = input
        .repos
        .iter()
        .find(|r| r.device == project.project.device && r.repo.path == project.project.path);
    if elsewhere || repo.is_none_or(|r| r.repo.touched_clipped) {
        return Err(refused());
    }
    avoid.extend(repo.map(|r| r.repo.touched.clone()).unwrap_or_default());
    Ok(avoid)
}

fn provider_name(p: Provider) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}
