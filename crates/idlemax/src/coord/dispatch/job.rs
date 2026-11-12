//! Adapter: how a pick becomes a worker job: the worker section from the pick, the task, the
//! project and `[workers]`, the reviewer from another provider, and the job that carries them.

use idlemax_core::dispatch::plan::Pick;
use idlemax_core::dispatch::project::ProjectView;
use idlemax_core::dispatch::task::Task;
use idlemax_core::quota::Provider;
use idlemax_core::types::{ActivePolicy, CostClass, JobSpec, UnixMs};
use idlemax_core::worker::{name, Baseline, Reviewer, WorkerAction, WorkerCli, WorkerSpec};

use super::LaunchStep;
use crate::dispatch::Gathered;

pub(super) fn worker_spec(
    step: &LaunchStep<'_>,
    view: &ProjectView,
    pick: &Pick,
    task: &Task,
    cli: WorkerCli,
    baseline: Option<Baseline>,
    reviewer: Option<Reviewer>,
) -> WorkerSpec {
    let w = &step.live.workers;
    let s = &view.project.settings;
    let scope = if pick.scope_paths.is_empty() {
        task.scope_paths.clone()
    } else {
        pick.scope_paths.clone()
    };
    WorkerSpec {
        run: 0,
        action: WorkerAction::Launch,
        project: view.project.name.clone(),
        task: name::task_slug(&task.id),
        task_title: task.title.clone(),
        done_check: task.done_check.clone().unwrap_or_default(),
        repo: view.project.path.clone(),
        base_branch: s.base_branch.clone(),
        cli,
        model: pick.model.clone(),
        setup: s.setup_command.clone(),
        check: s.check_command.clone(),
        baseline,
        scope_paths: scope,
        off_limits: pick.avoid_paths.clone(),
        core_paths: s.core_paths.clone(),
        reviewer,
        review_diff_lines: w.review_diff_lines,
        blocked_secs: w.blocked_secs,
        setup_secs: w.setup_secs,
        check_secs: w.check_secs,
        ..Default::default()
    }
}

/// The first usable account of another provider whose CLI may review, with its first model.
pub(super) fn reviewer(step: &LaunchStep<'_>, g: &Gathered, pick: &Pick) -> Option<Reviewer> {
    let models = step.live.dispatcher.models();
    g.plan
        .accounts
        .iter()
        .filter(|a| a.usable && a.provider != pick.provider)
        .find_map(|a| {
            let cli = cli_of(a.provider)?;
            step.live
                .workers
                .reviewers
                .iter()
                .any(|r| r == cli.as_str())
                .then_some(())?;
            let model = models.iter().find(|(p, _)| *p == a.provider)?.1.clone();
            Some(Reviewer {
                cli,
                model,
                account: a.account.clone(),
            })
        })
}

/// The worker job: the marker command no allowlist names, so an agent that does not know the
/// worker section refuses it (D6); the agent that knows it builds the real arguments.
#[allow(clippy::too_many_arguments)]
pub(super) fn job_spec(
    step: &LaunchStep<'_>,
    project: &str,
    task: &str,
    account: &str,
    priority: i32,
    device: &str,
    worker: WorkerSpec,
    brief: String,
) -> JobSpec {
    let run = u64::try_from(worker.run).unwrap_or(0);
    JobSpec {
        name: name::pane_name(project, task, run),
        key: Some(format!("run-{}", worker.run)),
        command: vec!["idlemax-worker".into(), worker.cli.as_str().into()],
        stdin: Some(brief),
        devices: vec![device.to_string()],
        priority,
        cost: CostClass::Ai,
        mem_mb: step.live.dispatcher.worker_mem_mib,
        max_runtime_secs: step.live.workers.max_runtime_secs,
        ai_account: Some(account.to_string()),
        on_user_active: ActivePolicy::Continue,
        max_attempts: 1,
        worker: Some(worker),
        ..Default::default()
    }
}

/// Workers start with Claude and Codex (A21).
pub(super) fn cli_of(provider: Provider) -> Option<WorkerCli> {
    match provider {
        Provider::Claude => Some(WorkerCli::Claude),
        Provider::Codex => Some(WorkerCli::Codex),
        _ => None,
    }
}

pub(super) fn secs_ms(secs: u64) -> UnixMs {
    i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX)
}
