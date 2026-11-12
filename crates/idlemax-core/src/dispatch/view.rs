//! Domain: `DispatcherView`, everything the owner looks at in one wire type (D12, D17), built
//! from the Plan, the Runs, the projects and the Budgets. The dashboard, the TUI, `idlemax
//! status` and the MCP status tool render it and compute nothing themselves. Pure.

use serde::{Deserialize, Serialize};

use super::label::{plain, run_label, run_summary, RunLine};
use super::plan::Plan;
use super::project::ProjectView;
use super::room::LineState;
use super::Mode;
use crate::config::Setting;
use crate::quota::budget::{Budget, BudgetBasis, BudgetUnit};
use crate::quota::owner_use::AccountDay;
use crate::quota::Provider;
use crate::types::{DeviceId, UnixMs};
use crate::worker::run::{Run, RunState};
use crate::worker::RunId;

/// Earlier account days shown beside today.
pub const EARLIER_DAYS: usize = 6;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatcherView {
    pub at_ms: UnixMs,
    pub mode: Mode,
    pub picked_by: String,
    pub devices: Vec<DeviceLine>,
    pub projects: Vec<ProjectLine>,
    pub planned: Vec<PlannedLine>,
    /// Ready tasks the orchestrator has not judged; no slot is filled from them (A19).
    pub ready_unjudged: Vec<TaskLine>,
    pub workers: Vec<WorkerLine>,
    pub needs_you: Vec<NeedsYouLine>,
    pub review: Vec<WorkerLine>,
    pub budgets: Vec<BudgetLine>,
    pub proposals_open: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceLine {
    pub id: DeviceId,
    /// None until Spec 5; no surface draws a device toggle for None.
    pub enabled: Option<bool>,
    pub state: LineState,
    pub reason: String,
    pub open_slots: u32,
    pub held: Option<String>,
    /// The device's own limits, read-only here (A23); empty until Spec 5 fills it.
    pub limits: Vec<Setting>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectLine {
    pub id: i64,
    pub name: String,
    pub device: DeviceId,
    pub enabled: bool,
    /// Runs in `clean` and `needs_you`, the two states counted against `review_cap` (A24).
    pub waiting: u32,
    pub review_cap: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannedLine {
    pub device: DeviceId,
    pub project: String,
    pub task: String,
    pub title: String,
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskLine {
    pub project: String,
    pub task: String,
    pub title: String,
}

/// One Run: a worker, or a finished task waiting for review.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerLine {
    pub run: RunId,
    pub device: DeviceId,
    pub project: String,
    pub task: String,
    pub state: RunState,
    pub label: String,
    pub summary: String,
    /// Why the task was picked (A5).
    pub why: String,
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub branch: Option<String>,
    pub start_ms: UnixMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedKind {
    #[default]
    Task,
    Account,
    Device,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NeedsYouLine {
    /// Stable across polls, so a surface can tell a new line from one it showed.
    pub key: String,
    pub kind: NeedKind,
    pub subject: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetLine {
    pub account: String,
    pub provider: Provider,
    pub unit: BudgetUnit,
    /// Today first, then up to `EARLIER_DAYS` earlier account days, newest first (A25).
    pub days: Vec<BudgetDay>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetDay {
    pub day_start_ms: UnixMs,
    pub day_end_ms: Option<UnixMs>,
    pub spendable_pct: Option<f64>,
    pub spent_pct: Option<f64>,
    pub left_pct: Option<f64>,
    pub why: Option<String>,
}

pub struct ViewInput<'a> {
    pub plan: &'a Plan,
    pub runs: &'a [RunLine],
    pub projects: &'a [ProjectView],
    pub budgets: &'a [Budget],
    pub days: &'a [AccountDay],
    /// The value of `workers.review_cap`.
    pub review_cap: u32,
    pub proposals_open: u64,
}

pub fn dispatcher_view(input: &ViewInput<'_>, now_ms: UnixMs) -> DispatcherView {
    let plan = input.plan;
    let devices: Vec<DeviceLine> = plan
        .devices
        .iter()
        .map(|d| DeviceLine {
            id: d.device.clone(),
            enabled: None,
            state: d.room.state,
            reason: plain(&d.room.reason),
            open_slots: d.room.open_slots,
            held: d.held.as_deref().map(plain),
            limits: Vec::new(),
        })
        .collect();
    let planned = plan
        .devices
        .iter()
        .flat_map(|d| d.picks.iter().map(move |p| (d, p)))
        .map(|(d, p)| PlannedLine {
            device: d.device.clone(),
            project: plain(&p.project),
            task: plain(&p.task),
            title: plain(&p.title),
            provider: p.provider,
            account: p.account.clone(),
            model: plain(&p.model),
            reason: plain(&p.reason),
        })
        .collect();
    let ready_unjudged = plan
        .not_judged
        .iter()
        .map(|t| TaskLine {
            project: plain(&t.project),
            task: plain(&t.id),
            title: plain(&t.title),
        })
        .collect();
    DispatcherView {
        at_ms: now_ms,
        mode: plan.mode,
        picked_by: plain(&plan.picked_by),
        projects: project_lines(input),
        planned,
        ready_unjudged,
        workers: run_lines(input.runs, is_worker, |r| r.start_ms),
        review: run_lines(input.runs, is_review, |r| r.updated_ms),
        needs_you: needs_you(plan, input.budgets, &devices, input.projects),
        budgets: budget_lines(input.budgets, input.days),
        devices,
        proposals_open: input.proposals_open,
    }
}

fn is_worker(state: RunState) -> bool {
    matches!(
        state,
        RunState::Starting
            | RunState::Running
            | RunState::Checking
            | RunState::Stopped
            | RunState::Shed
    )
}

fn is_review(state: RunState) -> bool {
    matches!(
        state,
        RunState::Clean | RunState::NeedsYou | RunState::Rejected
    )
}

/// The runs `keep` lets through, newest by `at` first.
fn run_lines(
    runs: &[RunLine],
    keep: fn(RunState) -> bool,
    at: fn(&Run) -> UnixMs,
) -> Vec<WorkerLine> {
    let mut lines: Vec<&RunLine> = runs.iter().filter(|l| keep(l.run.state)).collect();
    lines.sort_by_key(|l| std::cmp::Reverse((at(&l.run), l.run.id)));
    lines
        .into_iter()
        .map(|l| {
            let r = &l.run;
            WorkerLine {
                run: r.id,
                device: r.device.clone(),
                project: plain(&r.project),
                task: plain(&r.task),
                state: r.state,
                label: run_label(r.state, l.frozen).to_string(),
                summary: run_summary(l),
                why: plain(&r.reason),
                provider: r.provider,
                account: r.account.clone(),
                model: plain(&r.model),
                branch: r.branch.as_deref().map(plain),
                start_ms: r.start_ms,
            }
        })
        .collect()
}

fn project_lines(input: &ViewInput<'_>) -> Vec<ProjectLine> {
    input
        .projects
        .iter()
        .map(|v| {
            let p = &v.project;
            let waiting = input
                .runs
                .iter()
                .map(|l| &l.run)
                .filter(|r| r.project == p.name && r.device == p.device)
                .filter(|r| matches!(r.state, RunState::Clean | RunState::NeedsYou))
                .count();
            ProjectLine {
                id: p.id,
                name: plain(&p.name),
                device: p.device.clone(),
                enabled: p.enabled,
                waiting: u32::try_from(waiting).unwrap_or(u32::MAX),
                review_cap: input.review_cap,
            }
        })
        .collect()
}

/// What needs the owner, in this order: tasks the orchestrator sent to them, accounts out of
/// quota past today, then devices (see `device_needs`).
fn needs_you(
    plan: &Plan,
    budgets: &[Budget],
    devices: &[DeviceLine],
    projects: &[ProjectView],
) -> Vec<NeedsYouLine> {
    let tasks = plan.needs_you.iter().map(|t| NeedsYouLine {
        key: format!("task:{}:{}", t.project_id, t.id),
        kind: NeedKind::Task,
        subject: plain(&format!("{}: {}", t.project, t.title)),
        reason: plain(t.needs_you.as_deref().unwrap_or_default()),
    });
    let accounts = budgets
        .iter()
        .filter(|b| b.basis == BudgetBasis::Resting)
        .filter(|b| b.rests_until_ms.is_some_and(|until| until > b.day_end_ms))
        .map(|b| NeedsYouLine {
            key: format!("account:{}", b.account),
            kind: NeedKind::Account,
            subject: b.account.clone(),
            reason: plain(&b.why),
        });
    tasks
        .chain(accounts)
        .chain(device_needs(devices, projects))
        .collect()
}

/// Devices over the emergency line, then devices switched off while a project on them is on
/// (A23); the second kind stays empty until Spec 5 sets `enabled`.
fn device_needs(devices: &[DeviceLine], projects: &[ProjectView]) -> Vec<NeedsYouLine> {
    let emergencies = devices
        .iter()
        .filter(|d| d.state == LineState::Emergency)
        .map(|d| NeedsYouLine {
            key: format!("device:{}:emergency", d.id),
            kind: NeedKind::Device,
            subject: d.id.clone(),
            reason: plain(&d.reason),
        });
    let off = devices
        .iter()
        .filter(|d| d.enabled == Some(false))
        .filter_map(|d| {
            let on = projects
                .iter()
                .filter(|v| v.project.device == d.id && v.project.enabled)
                .count();
            (on > 0).then(|| NeedsYouLine {
                key: format!("device:{}:off", d.id),
                kind: NeedKind::Device,
                subject: d.id.clone(),
                reason: format!(
                    "off, with {on} projects switched on: idlemax device on {}",
                    d.id
                ),
            })
        });
    emergencies.chain(off).collect()
}

fn budget_lines(budgets: &[Budget], days: &[AccountDay]) -> Vec<BudgetLine> {
    budgets
        .iter()
        .map(|b| {
            let today = BudgetDay {
                day_start_ms: b.day_start_ms,
                day_end_ms: Some(b.day_end_ms),
                spendable_pct: Some(b.spendable_pct),
                spent_pct: Some(b.spent_pct),
                left_pct: Some(b.left_pct),
                why: Some(plain(&b.why)),
            };
            let mut earlier: Vec<&AccountDay> = days
                .iter()
                .filter(|d| d.account == b.account && d.day_start_ms < b.day_start_ms)
                .collect();
            earlier.sort_by_key(|d| std::cmp::Reverse(d.day_start_ms));
            let earlier = earlier.into_iter().take(EARLIER_DAYS).map(|d| BudgetDay {
                day_start_ms: d.day_start_ms,
                day_end_ms: None,
                spendable_pct: d.spendable_pct,
                spent_pct: d.idlemax_pct,
                left_pct: None,
                why: d.why.as_deref().map(plain),
            });
            BudgetLine {
                account: b.account.clone(),
                provider: b.provider,
                unit: b.unit,
                days: std::iter::once(today).chain(earlier).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
