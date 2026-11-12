//! Adapter: dispatcher use cases. The agent side reads Herdr and git on its own host (`herdr`,
//! `repo`, used by `agent::dispatch_feed`); the coordinator side (`step`) turns the stored
//! inputs into base checks, tasks and the plan through the pure `idlemax_core::dispatch` rules.

pub mod herdr;
pub mod repo;

use idlemax_core::dispatch::check::{check_job, next_step, BaseCheck, CheckStep, Phase};
use idlemax_core::dispatch::inputs::StoredRepo;
use idlemax_core::dispatch::project::Project;
use idlemax_core::dispatch::Mode;
use idlemax_core::types::{EventKind, UnixMs};
use idlemax_core::Config;

use crate::coord::store::{Added, Store, StoreError};

/// The coordinator's periodic dispatcher step: moves each switched-on project's base check
/// on by one step. Nothing runs in `off` mode. This is the one check-job mechanism (A11).
pub fn step(store: &mut Store, live: &Config, now: UnixMs) -> Result<(), StoreError> {
    if live.dispatcher.mode != Mode::Propose {
        return Ok(());
    }
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
            let cwd = repo
                .and_then(|r| r.repo.checkout.as_ref())
                .map(|c| c.path.clone())
                .unwrap_or_default();
            let spec = check_job(p, phase, &commit, &cwd);
            if let Err(e) = spec.validate() {
                let detail = format!("the {} job is not valid: {e}", phase.as_str());
                return store.put_base_check(&record(phase, None, None, Some(detail), Some(now)));
            }
            let job = match store.insert_job_once(&spec, now)? {
                Added::New(job) => {
                    let msg = format!("{} queued by the dispatcher", job.spec.name);
                    store.append_event(EventKind::JobQueued, None, Some(job.id), &msg, now)?;
                    job
                }
                Added::Existing(job) => job,
            };
            store.put_base_check(&record(phase, Some(job.id), None, None, None))
        }
    }
}
