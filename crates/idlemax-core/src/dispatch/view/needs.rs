//! Domain: the "needs you" lines of the dispatcher view: tasks sent to the owner, accounts
//! resting past today, devices in an emergency and devices switched off with a project on.
//! Pure.

use super::{DeviceLine, NeedKind, NeedsYouLine};
use crate::dispatch::label::plain;
use crate::dispatch::plan::Plan;
use crate::dispatch::project::ProjectView;
use crate::dispatch::room::LineState;
use crate::quota::budget::{Budget, BudgetBasis};

/// What needs the owner, in this order: tasks the orchestrator sent to them, accounts out of
/// quota past today, then devices (see `device_needs`).
pub(super) fn needs_you(
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
/// (A23).
pub(super) fn device_needs(devices: &[DeviceLine], projects: &[ProjectView]) -> Vec<NeedsYouLine> {
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
                    "off, with {on} {} switched on: idlemax device on {}",
                    if on == 1 { "project" } else { "projects" },
                    d.id
                ),
            })
        });
    emergencies.chain(off).collect()
}
