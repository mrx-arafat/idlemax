//! Adapter: dispatcher use cases. The agent side reads Herdr and git on its own host (`herdr`,
//! `repo`, used by `agent::dispatch_feed`); the coordinator side (`step`) turns the stored
//! inputs into base checks, tasks and the plan through the pure `idlemax_core::dispatch` rules.

mod gather;
pub mod herdr;
mod orchestrate;
pub mod pass;
mod quota_side;
pub mod repo;
pub mod rules_file;

pub use gather::{gather_plan, project_views, Gathered};
pub use orchestrate::ingest_pass;

use idlemax_core::dispatch::check::{check_job, next_step, BaseCheck, CheckStep, Phase};
use idlemax_core::dispatch::inputs::StoredRepo;
use idlemax_core::dispatch::project::Project;
use idlemax_core::dispatch::Mode;
use idlemax_core::types::{EventKind, JobId, UnixMs};
use idlemax_core::Config;

use crate::coord::store::{Added, Store, StoreError};

/// The coordinator's periodic dispatcher step: moves each switched-on project's base check
/// on by one step, then queues an orchestrator pass when one is due (`asked` for
/// `idlemax plan --refresh`). Nothing new runs in `off` mode, and a queued pass is cancelled.
/// This is the one check-job mechanism (A11).
pub fn step(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    rules: &str,
    now: UnixMs,
    asked: bool,
) -> Result<(), StoreError> {
    if live.dispatcher.mode != Mode::Off {
        base_checks_step(store, now)?;
    }
    orchestrate::pass_step(store, cfg, live, rules, now, asked)
}

/// One step of every switched-on project's base check.
fn base_checks_step(store: &mut Store, now: UnixMs) -> Result<(), StoreError> {
    let repos = store.dispatch_inputs()?;
    let checks = store.base_checks()?;
    for view in store.list_projects(None)? {
        let p = view.project;
        if !p.enabled {
            continue;
        }
        let repo = repos
            .iter()
            .find(|r| r.device == p.device && r.repo.path == p.path);
        let rec = checks.iter().find(|c| c.project == p.id);
        let job = match rec.and_then(|r| r.job) {
            Some(id) => store.get_job(id)?,
            None => None,
        };
        let next = next_step(&p, repo.map(|r| &r.repo), rec, job.as_ref());
        apply_check_step(store, &p, repo, next, now)?;
    }
    Ok(())
}

/// Queues the job of a phase, or writes the result.
fn apply_check_step(
    store: &mut Store,
    p: &Project,
    repo: Option<&StoredRepo>,
    next: CheckStep,
    now: UnixMs,
) -> Result<(), StoreError> {
    let commit = repo
        .and_then(|r| r.repo.base_commit.clone())
        .unwrap_or_default();
    let record = |phase, job, passed, detail, finished_ms| BaseCheck {
        project: p.id,
        base_commit: commit.clone(),
        phase,
        job,
        passed,
        detail,
        finished_ms,
    };
    match next {
        CheckStep::Settled | CheckStep::Wait(_) => Ok(()),
        CheckStep::Done { passed, detail } => {
            let phase = store
                .base_checks()?
                .into_iter()
                .find(|c| c.project == p.id)
                .map_or(Phase::Check, |c| c.phase);
            store.put_base_check(&record(phase, None, passed, Some(detail), Some(now)))
        }
        CheckStep::Queue(phase) => {
            let job = queue_phase(store, p, repo, phase, &commit, now)?;
            store.put_base_check(&match job {
                Ok(id) => record(phase, Some(id), None, None, None),
                Err(detail) => record(phase, None, None, Some(detail), Some(now)),
            })
        }
    }
}

/// Queues the job of a phase, or says why its spec is not valid.
fn queue_phase(
    store: &mut Store,
    p: &Project,
    repo: Option<&StoredRepo>,
    phase: Phase,
    commit: &str,
    now: UnixMs,
) -> Result<Result<JobId, String>, StoreError> {
    let cwd = repo
        .and_then(|r| r.repo.checkout.as_ref())
        .map(|c| c.path.clone())
        .unwrap_or_default();
    let spec = check_job(p, phase, commit, &cwd);
    if let Err(e) = spec.validate() {
        return Ok(Err(format!("the {} job is not valid: {e}", phase.as_str())));
    }
    let job = match store.insert_job_once(&spec, now)? {
        Added::New(job) => {
            let msg = format!("{} queued by the dispatcher", job.spec.name);
            store.append_event(EventKind::JobQueued, None, Some(job.id), &msg, now)?;
            job
        }
        Added::Existing(job) => job,
    };
    Ok(Ok(job.id))
}
