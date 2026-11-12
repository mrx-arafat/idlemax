//! Domain: the "needs you" lines of the dispatcher view: tasks sent to the owner, runs that
//! changed the repository's git config or hooks or idlemax's own files, accounts resting past
//! today, devices in an emergency and devices switched off with a project on. Pure.

use super::{DeviceLine, NeedKind, NeedsYouLine};
use crate::dispatch::label::{plain, RunLine};
use crate::dispatch::plan::Plan;
use crate::dispatch::project::ProjectView;
use crate::dispatch::room::LineState;
use crate::quota::budget::{Budget, BudgetBasis};
use crate::quota::identity::AccountProblem;
use crate::worker::checks::{tampered_concern, GIT_CHANGED};
use crate::worker::run::RunState;

/// What needs the owner, in this order: tasks the orchestrator sent to them, rejected runs
/// that changed the git config or hooks or idlemax's own files, accounts out of quota past
/// today, accounts their logins close, then devices (see `device_needs`).
pub(super) fn needs_you(
    plan: &Plan,
    runs: &[RunLine],
    budgets: &[Budget],
    problems: &[AccountProblem],
    devices: &[DeviceLine],
    projects: &[ProjectView],
) -> Vec<NeedsYouLine> {
    let tasks = plan.needs_you.iter().map(|t| NeedsYouLine {
        key: format!("task:{}:{}", t.project_id, t.id),
        kind: NeedKind::Task,
        subject: plain(&format!("{}: {}", t.project, t.title)),
        reason: plain(t.needs_you.as_deref().unwrap_or_default()),
    });
    let changed_git = runs
        .iter()
        .map(|l| &l.run)
        .filter(|r| r.state == RunState::Rejected)
        .filter(|r| r.facts.check.as_ref().is_some_and(|f| f.git_changed))
        .map(|r| NeedsYouLine {
            key: format!("run:{}:git", r.id),
            kind: NeedKind::Task,
            subject: plain(&format!("{}: {}", r.project, r.task_title)),
            reason: format!(
                "{GIT_CHANGED}: look at its repository, then idlemax review discard {}",
                r.id
            ),
        });
    // A Run that changed idlemax's own files is rejected, and the owner must look at once.
    let tampered = runs.iter().filter_map(|l| {
        let files = &l.run.facts.check.as_ref()?.tampered;
        (!files.is_empty()).then(|| NeedsYouLine {
            key: format!("run:{}:tampered", l.run.id),
            kind: NeedKind::Task,
            subject: plain(&format!("{}: {}", l.run.project, l.run.task_title)),
            reason: plain(&format!("{}; check them", tampered_concern(files))),
        })
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
    let closed = problems.iter().flat_map(|p| {
        p.closed_names().iter().map(move |name| NeedsYouLine {
            key: format!("account:{name}:{}", p.kind.as_str()),
            kind: NeedKind::Account,
            subject: name.clone(),
            reason: plain(&format!("{}: {}", p.message, p.fix)),
        })
    });
    tasks
        .chain(changed_git)
        .chain(tampered)
        .chain(accounts)
        .chain(closed)
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
