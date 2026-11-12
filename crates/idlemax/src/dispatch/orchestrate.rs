//! Adapter: the pass part of the dispatcher step and the ingest of a pass's answer. The pure
//! rules decide when a pass is due (A20) and which picks stand; this file queues the job,
//! records the pass and its judgments, and cancels a pass that stopped being due.

use idlemax_core::control::plan_control;
use idlemax_core::dispatch::orchestrator::{
    fingerprint, is_pass, pass_due, schema, taking_devices, validate, DueInput, DuePass,
    FingerprintParts, Offer, PASS_ACTIVE,
};
use idlemax_core::dispatch::plan::PassResult;
use idlemax_core::dispatch::prompt::prompt;
use idlemax_core::dispatch::proposal::{admit, RuleProposal};
use idlemax_core::dispatch::task::{digest, Task};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::quota::pass::any_room;
use idlemax_core::types::{ControlAction, ControlScope, EventKind, JobId, JobStatus, UnixMs};
use idlemax_core::Config;

use super::gather::{read_inputs, Inputs};
use super::pass::pass_job;
use crate::advisor::claude::structured_output;
use crate::coord::store::{Added, Store, StoreError};

/// Queues a pass when one is due, cancels a queued one that stopped being due, records a pass
/// job that ended without a result, and forgets judgments whose task changed.
pub fn pass_step(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    rules: &str,
    now: UnixMs,
    asked: bool,
) -> Result<(), StoreError> {
    let inputs = read_inputs(store, cfg, live, now)?;
    forget_changed_judgments(store, &inputs.tasks)?;
    judge_twice_failed(store, &inputs, now)?;
    record_failed_pass(store, &inputs, now)?;
    let (tasks, accounts) = inputs.offered();
    let quota = inputs.quota.offer();
    let finished = store.finished_runs()?;
    let offer = Offer {
        tasks: &tasks,
        accounts: &accounts,
        models: &inputs.models,
        quota: Some(&quota),
        finished: &finished,
    };
    let print = pass_fingerprint(store, &inputs, &offer, rules)?;
    match due_now(&inputs, &offer, &print, asked) {
        Ok(due) => {
            let text = prompt(
                rules,
                &inputs.devices,
                &inputs.paused,
                &inputs.projects,
                &inputs.repos,
                &offer,
            );
            queue_pass(store, &due, text, &print, &tasks, now)
        }
        Err(why) if why != PASS_ACTIVE => cancel_queued_pass(store, &why, now),
        Err(_) => Ok(()),
    }
}

/// Whether a pass is due now (A20), and on which account and device.
fn due_now(
    inputs: &Inputs,
    offer: &Offer<'_>,
    print: &str,
    asked: bool,
) -> Result<DuePass, String> {
    let newest = inputs
        .newest
        .as_ref()
        .map(|r| (r.fingerprint.as_str(), r.queued_ms));
    pass_due(&DueInput {
        now_ms: inputs.now,
        mode: inputs.mode,
        asked,
        devices: &inputs.devices,
        paused: &inputs.paused,
        offer,
        fingerprint: print,
        newest,
        newest_offered: inputs.newest.as_ref().map_or(&[], |r| r.offered.as_slice()),
        pass_active: inputs.pass_active,
        room: any_room(&inputs.quota.offers),
        room_off_clis: inputs.quota.room_off_clis,
    })
}

/// Queues the pass job and records what it was offered.
fn queue_pass(
    store: &mut Store,
    due: &DuePass,
    prompt_text: String,
    print: &str,
    tasks: &[&Task],
    now: UnixMs,
) -> Result<(), StoreError> {
    let spec = pass_job(due, prompt_text, &schema());
    spec.validate().map_err(StoreError::InvalidSpec)?;
    if let Added::New(job) = store.insert_job_once(&spec, now)? {
        let msg = format!("{} queued on {}", job.spec.name, due.account);
        store.append_event(EventKind::JobQueued, None, Some(job.id), &msg, now)?;
        let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        store.insert_pass(job.id, &due.account, print, &ids, now)?;
    }
    Ok(())
}

/// The fingerprint of A20: the devices that take picks, every ready task and every task a
/// stored judgment holds (so a judgment itself never looks like a change), the usable
/// accounts, the models and the rules.
fn pass_fingerprint(
    store: &Store,
    inputs: &Inputs,
    offer: &Offer<'_>,
    rules: &str,
) -> Result<String, StoreError> {
    let judged = store.judgments()?;
    let tasks: Vec<(String, String)> = inputs
        .tasks
        .iter()
        .filter(|t| {
            t.needs_you.is_none()
                || judged
                    .iter()
                    .any(|j| j.task == t.id && j.digest == digest(t))
        })
        .map(|t| (t.id.clone(), digest(t)))
        .collect();
    let devices: Vec<String> = taking_devices(&inputs.devices, &inputs.paused)
        .iter()
        .map(|d| d.info.id.clone())
        .collect();
    let accounts: Vec<String> = offer.accounts.iter().map(|a| a.account.clone()).collect();
    Ok(fingerprint(&FingerprintParts {
        devices: &devices,
        tasks: &tasks,
        accounts: &accounts,
        models: &inputs.models,
        rules,
    }))
}

/// A judgment lasts exactly until its task changes or goes (A20).
fn forget_changed_judgments(store: &mut Store, tasks: &[Task]) -> Result<(), StoreError> {
    for j in store.judgments()? {
        let current = tasks.iter().find(|t| t.id == j.task);
        if current.is_none_or(|t| digest(t) != j.digest) {
            store.delete_judgment(&j.task)?;
        }
    }
    Ok(())
}

/// D10.6: a task whose checks failed on two Runs needs the owner; the judgment takes it out
/// of the offer until the task changes.
fn judge_twice_failed(store: &mut Store, inputs: &Inputs, now: UnixMs) -> Result<(), StoreError> {
    for (id, failed) in &inputs.quota.failed {
        let task = inputs.tasks.iter().find(|t| t.id == *id);
        if let Some(t) = task.filter(|_| *failed >= 2) {
            store.judge_task(id, &digest(t), FAILED_TWICE, now)?;
        }
    }
    Ok(())
}

/// The "needs you" reason of a task that failed its checks twice.
pub const FAILED_TWICE: &str = "failed its checks twice";

/// A pass job that ended without a result writes how it ended into its row.
fn record_failed_pass(store: &mut Store, inputs: &Inputs, now: UnixMs) -> Result<(), StoreError> {
    let Some(row) = inputs
        .newest
        .as_ref()
        .filter(|r| r.result.is_none() && r.error.is_none())
    else {
        return Ok(());
    };
    let Some(job) = store.get_job(row.job)? else {
        return Ok(());
    };
    if !job.status.is_terminal() || job.status == JobStatus::Succeeded {
        return Ok(());
    }
    let mut why = format!("the pass job {}", job.status);
    if let Some(e) = &job.last_error {
        why.push_str(&format!(": {e}"));
    }
    store.finish_pass(row.id, Err(&why), now)
}

/// Cancels a queued pass job that is no longer due, with the reason.
fn cancel_queued_pass(store: &mut Store, why: &str, now: UnixMs) -> Result<(), StoreError> {
    let queued = store.queued_jobs()?;
    let Some(job) = queued.iter().find(|j| is_pass(&j.spec)) else {
        return Ok(());
    };
    let req = ControlRequest {
        scope: ControlScope::Job(job.id),
        action: ControlAction::Cancel,
        reason: Some(format!("no longer due: {why}")),
    };
    let Ok(plan) = plan_control(&req, std::slice::from_ref(job), now) else {
        return Ok(());
    };
    store.apply_control(&plan, now)?;
    let msg = format!("{} cancelled: no longer due: {why}", job.spec.name);
    store.append_event(EventKind::Control, None, Some(job.id), &msg, now)?;
    Ok(())
}

/// Reads and checks a finished pass's answer against what is offered now, stores the result
/// or the error, and sends the tasks it judged to "needs you". A row that already has a
/// result or an error is left alone, so a replayed update changes nothing.
pub fn ingest_pass(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    rules: &str,
    job: JobId,
    output: &str,
    now: UnixMs,
) -> Result<(), StoreError> {
    let Some(row) = store.pass_for_job(job)? else {
        return Ok(());
    };
    if row.result.is_some() || row.error.is_some() {
        return Ok(());
    }
    let inputs = read_inputs(store, cfg, live, now)?;
    let (tasks, accounts) = inputs.offered();
    let quota = inputs.quota.offer();
    let finished = store.finished_runs()?;
    let offer = Offer {
        tasks: &tasks,
        accounts: &accounts,
        models: &inputs.models,
        quota: Some(&quota),
        finished: &finished,
    };
    let checked = structured_output(output)
        .map_err(|e| e.to_string())
        .and_then(|value| validate(&value, &offer).map_err(|e| e.to_string()));
    let mut result = match checked {
        Ok(result) => result,
        Err(e) => {
            store.append_event(
                EventKind::Error,
                None,
                Some(job),
                &format!("orchestrator pass rejected: {e}"),
                now,
            )?;
            return store.finish_pass(row.id, Err(&e), now);
        }
    };
    result.seen = row.offered.clone();
    store.finish_pass(row.id, Ok(&result), now)?;
    record_result(store, &inputs.tasks, &result, rules, job, now)
}

/// Sends the tasks the pass judged to "needs you", and logs what it returned.
fn record_result(
    store: &mut Store,
    tasks: &[Task],
    result: &PassResult,
    rules: &str,
    job: JobId,
    now: UnixMs,
) -> Result<(), StoreError> {
    for (task, reason) in &result.needs_you {
        if let Some(t) = tasks.iter().find(|t| t.id == *task) {
            store.judge_task(task, &digest(t), reason, now)?;
        }
    }
    if let Some(p) = &result.rule_proposal {
        keep_proposal(store, p, rules, job, now)?;
    }
    let msg = format!(
        "orchestrator pass: {} picks, {} sent to needs you, {} dropped",
        result.picks.len(),
        result.needs_you.len(),
        result.dropped.len()
    );
    store.append_event(EventKind::Control, None, Some(job), &msg, now)?;
    Ok(())
}

/// Stores a rule proposal the domain admits; a refused one is dropped with one event.
fn keep_proposal(
    store: &mut Store,
    p: &RuleProposal,
    rules: &str,
    job: JobId,
    now: UnixMs,
) -> Result<(), StoreError> {
    let state = store.get_run(p.run)?.map(|r| r.state);
    match admit(p, state, rules, &store.proposals()?) {
        Ok(()) => {
            store.insert_proposal(p, now)?;
            let msg = format!("rule proposal for run {}: {}", p.run, p.line);
            store.append_event(EventKind::Control, None, Some(job), &msg, now)?;
        }
        Err(why) => {
            let msg = format!("rule proposal for run {} dropped: {why}", p.run);
            store.append_event(EventKind::Info, None, Some(job), &msg, now)?;
        }
    }
    Ok(())
}
