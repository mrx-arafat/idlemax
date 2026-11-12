//! Adapter: the `auto` step (D3). On each sweep it stops the workers of an account that may no
//! longer be used, then per device relaunches the oldest Run back in the queue, or else
//! turns the next orchestrator pick into a Run and a worker job leased to that device: one
//! launch per device at a time, inside the room, the account rule and the review cap.
//! Decisions are the pure `worker::pace` and `worker::control`; this reads and writes.

use std::collections::HashMap;

use idlemax_core::control::paused_accounts;
use idlemax_core::dispatch::accounts::{AccountUse, MEASURING_WEEK_CAP_PCT};
use idlemax_core::dispatch::plan::{DevicePlan, Pick, PickedBy};
use idlemax_core::dispatch::task::Task;
use idlemax_core::dispatch::Mode;
use idlemax_core::quota::Provider;
use idlemax_core::scheduler::lease_deadline;
use idlemax_core::types::{ControlScope, EventKind, Job, UnixMs};
use idlemax_core::worker::brief::brief;
use idlemax_core::worker::control::plan_stop;
use idlemax_core::worker::pace::{may_launch, task_open, under_cap, LaunchGate, TaskOpen};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};
use idlemax_core::worker::{Baseline, RunId, WorkerAction, WorkerSpec};
use idlemax_core::Config;

use crate::coord::api::offline_after_ms;
use crate::coord::store::{Store, StoreError};
use crate::dispatch::{gather_plan, project_views, spend, Gathered};
use job::{cli_of, job_spec, reviewer, secs_ms, worker_spec};

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
    stop_spent_accounts(store, step, &gathered.plan.accounts)?;
    for device in &gathered.plan.devices {
        launch_on(store, step, &gathered, device, refusals)?;
    }
    Ok(())
}

/// Stops, with their state saved, the live workers of an account at 60 percent of its
/// 5-hour window or at the measuring-week cap; their Runs are `shed` and continue when the
/// account allows it (A18). The operation Spec 3 calls for rate limits.
fn stop_spent_accounts(
    store: &mut Store,
    step: &LaunchStep<'_>,
    accounts: &[AccountUse],
) -> Result<(), StoreError> {
    let spent = spend::spent_week(store, step.cfg, step.now)?;
    for a in accounts {
        let five = a.five_hour_used_pct.unwrap_or(0.0);
        let week = spent
            .iter()
            .find(|(n, _)| *n == a.account)
            .map_or(0.0, |(_, p)| *p);
        let why = if five >= FIVE_HOUR_STOP_PCT {
            format!("{}: 5-hour window {five:.0}% used", a.account)
        } else if week >= MEASURING_WEEK_CAP_PCT {
            format!("{}: {week:.1}% of the weekly quota spent", a.account)
        } else {
            continue;
        };
        stop_workers(
            store,
            &ControlScope::Account(a.account.clone()),
            &why,
            step.now,
        )?;
    }
    Ok(())
}

/// The 5-hour line of the simple account rule; the Budget's `stops_workers` replaces this
/// stop site in Spec 3 Task 9.
const FIVE_HOUR_STOP_PCT: f64 = 60.0;

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
    let runs = store.list_runs(None, None, 0)?;
    let device_runs: Vec<Run> = runs
        .iter()
        .filter(|r| r.device == device.device)
        .cloned()
        .collect();
    let views = store.devices(step.now, offline_after_ms(step.cfg))?;
    let reported: Vec<i64> = views
        .iter()
        .find(|d| d.info.id == device.device)
        .map(|d| d.running.iter().map(|r| r.id).collect())
        .unwrap_or_default();
    let paused = paused_accounts(&store.paused_scopes()?);
    let last_launch_ms = store.last_worker_launch(&device.device)?;
    let workers = &step.live.workers;
    let gate = |provider: Provider, account: &str| LaunchGate {
        now_ms: step.now,
        held: device.held.as_deref(),
        account_paused: paused.iter().any(|p| p == account),
        room: &device.room,
        device_runs: &device_runs,
        last_launch_ms,
        settle_ms: secs_ms(workers.settle_secs),
        reported: &reported,
        cli_allowed: cli_of(provider).is_some_and(|c| workers.clis.iter().any(|w| w == c.as_str())),
        account: g.plan.accounts.iter().find(|a| a.account == account),
    };
    let mut shed: Vec<&Run> = device_runs
        .iter()
        .filter(|r| r.state == RunState::Shed)
        .collect();
    shed.sort_by_key(|r| (r.start_ms, r.id));
    for run in shed {
        match may_launch(&gate(run.provider, &run.account), run.job) {
            None => return relaunch(store, step, run),
            Some(why) => refusals.note(store, format!("run {}", run.id), &why, step.now)?,
        }
    }
    for pick in &device.picks {
        let key = format!("{}: {}", pick.project, pick.title);
        let PickedBy::Orchestrator = pick.by;
        let Some(task) = g.tasks.iter().find(|t| t.id == pick.task) else {
            continue;
        };
        let why = match task_open(&runs, &task.id) {
            TaskOpen::Busy => continue,
            TaskOpen::Closed => {
                Some("its last two runs were rejected or discarded: it needs you".into())
            }
            TaskOpen::Open => under_cap(&runs, &pick.project, workers.review_cap)
                .or_else(|| may_launch(&gate(pick.provider, &pick.account), None)),
        };
        match why {
            None => return launch_pick(store, step, g, device, pick, task),
            Some(why) => refusals.note(store, key, &why, step.now)?,
        }
    }
    Ok(())
}

fn launch_pick(
    store: &mut Store,
    step: &LaunchStep<'_>,
    g: &Gathered,
    device: &DevicePlan,
    pick: &Pick,
    task: &Task,
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
        reviewer(step, g, pick),
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

mod job;
#[cfg(test)]
mod tests;
