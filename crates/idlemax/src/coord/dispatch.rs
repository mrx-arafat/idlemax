//! Adapter: the `auto` step (D3). On each sweep it stops the workers of an account that may no
//! longer be used, then per device relaunches the oldest Run back in the queue, or else
//! turns the next orchestrator pick into a Run and a worker job leased to that device: one
//! launch per device at a time, inside the room, the worker gate of each Budget and the review cap.
//! Decisions are the pure `worker::pace` and `worker::control`; this reads and writes.

use std::collections::HashMap;

use idlemax_core::dispatch::plan::{DevicePlan, Pick, PickedBy};
use idlemax_core::dispatch::task::Task;
use idlemax_core::dispatch::Mode;
use idlemax_core::quota::identity::usable_on;
use idlemax_core::quota::spend::stops_workers;
use idlemax_core::scheduler::lease_deadline;
use idlemax_core::types::{ControlScope, EventKind, Job, UnixMs};
use idlemax_core::worker::brief::brief;
use idlemax_core::worker::control::plan_stop;
use idlemax_core::worker::pace::{task_open, under_cap, TaskOpen};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};
use idlemax_core::worker::{Baseline, RunId, WorkerAction, WorkerSpec};
use idlemax_core::Config;

use crate::coord::api::{budgets, quota_views, Gates};
use crate::coord::store::{Store, StoreError};
use crate::dispatch::{gather_plan, project_views, Gathered};
use facts::DeviceFacts;
use job::{cli_of, job_spec, reviewer, worker_spec};

/// The last refusal recorded per Run or pick, so a slot refused on every sweep for the same
/// reason leaves one event, not 5,760 a day.
#[derive(Debug, Default)]
pub struct Refusals(HashMap<String, String>);

impl Refusals {
    fn note(
        &mut self,
        store: &mut Store,
        key: String,
        why: &str,
        now: UnixMs,
    ) -> Result<(), StoreError> {
        if self.0.get(&key).map(String::as_str) == Some(why) {
            return Ok(());
        }
        self.0.insert(key.clone(), why.to_string());
        let msg = format!("{key} not launched: {why}");
        tracing::info!("{msg}");
        store.append_event(EventKind::Control, None, None, &msg, now)?;
        Ok(())
    }
}

/// What one launch step reads besides the store.
pub struct LaunchStep<'a> {
    pub cfg: &'a Config,
    /// The newest `config.toml`: `[dispatcher]` and `[workers]` (A2).
    pub live: &'a Config,
    pub rules: &'a str,
    pub now: UnixMs,
}

/// One `auto` step. Nothing happens unless the mode is `auto` and no owner stop is stored.
pub fn launch_due(
    store: &mut Store,
    step: &LaunchStep<'_>,
    refusals: &mut Refusals,
) -> Result<(), StoreError> {
    if step.live.dispatcher.mode != Mode::Auto || store.worker_stop()?.is_some() {
        return Ok(());
    }
    let gathered = gather_plan(store, step.cfg, step.live, step.now)?;
    stop_closed_accounts(store, step)?;
    for device in &gathered.plan.devices {
        launch_on(store, step, &gathered, device, refusals)?;
    }
    Ok(())
}

/// Stops, with their state saved, the live workers of every account whose Budget says so:
/// resting after a usage limit, or at the 5-hour stop (A18), never for a day that is spent.
/// Their Runs are `shed`, and the worker gate relaunches them once the Budget is usable
/// again (Spec 3 Task 9).
fn stop_closed_accounts(store: &mut Store, step: &LaunchStep<'_>) -> Result<(), StoreError> {
    let views = quota_views(store, step.cfg, step.now)?;
    for b in budgets(store, step.cfg, step.live, &views, step.now)? {
        if stops_workers(b.basis) {
            let why = format!("{}: {}", b.account, b.why);
            stop_workers(store, &ControlScope::Account(b.account), &why, step.now)?;
        }
    }
    Ok(())
}

/// Stops the live workers of `scope` with their state saved: `stopped` for the owner's global
/// stop, `shed` for an account.
pub fn stop_workers(
    store: &mut Store,
    scope: &ControlScope,
    why: &str,
    now: UnixMs,
) -> Result<Vec<Run>, StoreError> {
    let runs = store.list_runs(None, None, 0)?;
    let moved = plan_stop(&runs, scope, now).map_err(|e| StoreError::Invalid(e.to_string()))?;
    let done = store.stop_runs(&moved, why, now)?;
    if !done.is_empty() {
        let msg = format!("{} workers stopped: {why}", done.len());
        store.append_event(EventKind::Control, None, None, &msg, now)?;
    }
    Ok(done)
}

/// The oldest Run of the device back in the queue that may launch, else the first pick that
/// may: at most one launch per device and sweep.
fn launch_on(
    store: &mut Store,
    step: &LaunchStep<'_>,
    g: &Gathered,
    device: &DevicePlan,
    refusals: &mut Refusals,
) -> Result<(), StoreError> {
    let facts = DeviceFacts::read(store, step, &device.device)?;
    let mut shed: Vec<&Run> = facts
        .device_runs
        .iter()
        .filter(|r| r.state == RunState::Shed)
        .collect();
    shed.sort_by_key(|r| (r.start_ms, r.id));
    for run in shed {
        let account = facts.account(g, &run.account, facts.gates.relaunch(&run.account));
        // An account its logins close on this device (Spec 5 Task 6) gets no relaunch there.
        let why = usable_on(&g.problems, &device.device, &run.account)
            .err()
            .or_else(|| facts.may_launch(step, device, run.provider, account.as_ref(), run.job));
        match why {
            None => return relaunch(store, step, run),
            Some(why) => refusals.note(store, format!("run {}", run.id), &why, step.now)?,
        }
    }
    for pick in &device.picks {
        let key = format!("{}: {}", pick.project, pick.title);
        let Some(task) = g.tasks.iter().find(|t| t.id == pick.task) else {
            continue;
        };
        match pick_hold(step, g, device, &facts, pick, task) {
            None => return launch_pick(store, step, g, device, pick, task, &facts.gates),
            Some(why) if why == IN_FLIGHT => continue,
            Some(why) => refusals.note(store, key, &why, step.now)?,
        }
    }
    Ok(())
}

const IN_FLIGHT: &str = "a run of this task is in flight";

/// Why the launch step holds `pick` now; None when it may launch.
fn pick_hold(
    step: &LaunchStep<'_>,
    g: &Gathered,
    device: &DevicePlan,
    facts: &DeviceFacts,
    pick: &Pick,
    task: &Task,
) -> Option<String> {
    let PickedBy::Orchestrator = pick.by;
    match task_open(&facts.runs, &task.id) {
        TaskOpen::Busy => Some(IN_FLIGHT.into()),
        TaskOpen::Closed => {
            Some("its last two runs were rejected or discarded: it needs you".into())
        }
        TaskOpen::Open => under_cap(&facts.runs, &pick.project, step.live.workers.review_cap)
            .or_else(|| {
                let verdict = facts
                    .gates
                    .start(&pick.account, &pick.model, task.source.as_str());
                let account = facts.account(g, &pick.account, verdict);
                facts.may_launch(step, device, pick.provider, account.as_ref(), None)
            }),
    }
}

/// The plan, with each pick in `auto` saying why the launch step holds it, so a pick that
/// never launches is not silent (the owner's stop holds every pick the same way).
pub fn plan_with_holds(
    store: &Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<Gathered, StoreError> {
    let mut g = gather_plan(store, cfg, live, now)?;
    if live.dispatcher.mode != Mode::Auto {
        return Ok(g);
    }
    let stop = store
        .worker_stop()?
        .map(|(_, why)| format!("stopped by you: {why}"));
    let step = LaunchStep {
        cfg,
        live,
        rules: "",
        now,
    };
    let mut held = Vec::new();
    for device in &g.plan.devices {
        let facts = DeviceFacts::read(store, &step, &device.device)?;
        for pick in &device.picks {
            let task = g.tasks.iter().find(|t| t.id == pick.task);
            let why = match (&stop, task) {
                (Some(stop), _) => Some(stop.clone()),
                (None, Some(task)) => pick_hold(&step, &g, device, &facts, pick, task),
                (None, None) => None,
            };
            held.push(why);
        }
    }
    let picks = g.plan.devices.iter_mut().flat_map(|d| d.picks.iter_mut());
    for (pick, why) in picks.zip(held) {
        pick.held = why;
    }
    Ok(g)
}

fn launch_pick(
    store: &mut Store,
    step: &LaunchStep<'_>,
    g: &Gathered,
    device: &DevicePlan,
    pick: &Pick,
    task: &Task,
    gates: &Gates,
) -> Result<(), StoreError> {
    let Some(view) = project_views(store, None, step.now)?
        .into_iter()
        .find(|v| v.project.name == pick.project && v.project.device == device.device)
    else {
        return Ok(());
    };
    let Some(cli) = cli_of(pick.provider) else {
        return Ok(());
    };
    let baseline = store
        .base_checks()?
        .into_iter()
        .find(|c| c.project == view.project.id && c.passed.is_some())
        .map(|c| Baseline {
            base_commit: c.base_commit,
            passed: c.passed,
        });
    let worker = worker_spec(
        step,
        &view,
        pick,
        task,
        cli,
        baseline,
        reviewer(step, g, pick, gates),
    );
    let text = brief(&worker, step.rules);
    let launch = RunLaunch {
        worker: worker.clone(),
        brief: text.clone(),
    };
    let account = pick.account.clone();
    let run = Run {
        device: device.device.clone(),
        project: pick.project.clone(),
        task: task.id.clone(),
        task_title: task.title.clone(),
        source: task.source.as_str().into(),
        provider: pick.provider,
        account: account.clone(),
        model: pick.model.clone(),
        state: RunState::Starting,
        reason: pick.reason.clone(),
        start_ms: step.now,
        updated_ms: step.now,
        quota_before: store
            .latest_quota()?
            .into_iter()
            .filter(|s| s.account == account)
            .collect(),
        ..Default::default()
    };
    let priority = view.project.settings.priority;
    let make = |id: RunId| {
        let worker = WorkerSpec { run: id, ..worker };
        job_spec(
            step,
            &view.project.name,
            &task.id,
            &account,
            priority,
            &device.device,
            worker,
            text,
        )
    };
    let expires = lease_deadline(step.cfg.coordinator.lease_secs, step.now);
    let (run, job) = store.launch_run(&run, &launch, make, expires, step.now)?;
    launched(store, &run, &job, step.now)
}

/// Relaunches a Run back in the queue: a `resume` when its session is stored, else a
/// `launch` over its existing worktree.
fn relaunch(store: &mut Store, step: &LaunchStep<'_>, run: &Run) -> Result<(), StoreError> {
    let Some(launch) = store.run_launch(run.id)? else {
        return Ok(());
    };
    let worker = WorkerSpec {
        // The stored launch predates the Run's id; without it every relaunch shares the key
        // `run-0` and a second one is refused as a duplicate.
        run: run.id,
        action: if run.session.is_some() {
            WorkerAction::Resume
        } else {
            WorkerAction::Launch
        },
        branch: run.branch.clone(),
        worktree: run.worktree.clone(),
        session: run.session.clone(),
        ..launch.worker
    };
    let priority = store
        .list_projects(Some(&run.device))?
        .into_iter()
        .find(|v| v.project.name == run.project)
        .map_or(0, |v| v.project.settings.priority);
    let spec = job_spec(
        step,
        &run.project,
        &run.task,
        &run.account,
        priority,
        &run.device,
        worker,
        launch.brief,
    );
    let expires = lease_deadline(step.cfg.coordinator.lease_secs, step.now);
    match store.relaunch_run(run, RunState::Shed, &spec, expires, step.now)? {
        Some((run, job)) => launched(store, &run, &job, step.now),
        None => Ok(()),
    }
}

/// The event that says what was launched and why (D3).
fn launched(store: &mut Store, run: &Run, job: &Job, now: UnixMs) -> Result<(), StoreError> {
    let what = if run.launches > 1 {
        "relaunched"
    } else {
        "launched"
    };
    let msg = format!(
        "worker {what}: {}: {} on {}/{} ({}): {}",
        run.project,
        run.task_title,
        run.provider.cli().unwrap_or("api"),
        run.model,
        run.account,
        run.reason
    );
    store.append_event(
        EventKind::JobLeased,
        Some(&run.device),
        Some(job.id),
        &msg,
        now,
    )?;
    Ok(())
}

#[cfg(test)]
mod device_tests;
mod facts;
mod job;
#[cfg(test)]
mod tests;
