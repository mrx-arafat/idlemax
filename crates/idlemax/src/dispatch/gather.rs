//! Adapter: reads everything the plan needs under one store lock and builds it through the
//! pure rules (`discover`, `account_uses`, `build`). Writes nothing.

use idlemax_core::advice::{merge_inputs, SessionMeta};
use idlemax_core::control::paused_accounts;
use idlemax_core::dispatch::accounts::account_uses;
use idlemax_core::dispatch::check::BaseCheck;
use idlemax_core::dispatch::inputs::StoredRepo;
use idlemax_core::dispatch::plan::{build, Plan, PlanInput};
use idlemax_core::dispatch::project::{owner_active, ProjectView, MAX_PROJECTS_PER_DEVICE};
use idlemax_core::dispatch::room::LineState;
use idlemax_core::dispatch::task::{discover, Task, TaskInput};
use idlemax_core::protocol::DeviceView;
use idlemax_core::types::{DeviceId, UnixMs};
use idlemax_core::Config;

use crate::coord::api::{offline_after_ms, quota_views};
use crate::coord::store::{Store, StoreError};

/// The Plan and every task `discover` returned.
#[derive(Debug, Clone)]
pub struct Gathered {
    pub plan: Plan,
    #[expect(dead_code, reason = "the orchestrator pass reads it next")]
    pub tasks: Vec<Task>,
}

/// `cfg` is the coordinator's start config and gives `[quota]`; `live` is the newest
/// `config.toml` and gives `[dispatcher]`, so a mode change needs no restart.
pub fn gather_plan(
    store: &Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<Gathered, StoreError> {
    let devices = store.devices(now, offline_after_ms(cfg))?;
    let paused = store.paused_scopes()?;
    let projects = project_views(store, None, now)?;
    let repos = store.dispatch_inputs()?;
    let checks = store.base_checks()?;
    let (github, _) = merge_inputs(&store.advisor_inputs()?, now);
    let tasks = discover(&TaskInput {
        now_ms: now,
        projects: &projects,
        repos: &repos,
        checks: &checks,
        issues: &github.assigned_issues,
        sessions: &sessions(store)?,
        judgments: &[],
        scoped: &[],
    });
    let views = quota_views(store, cfg, now)?;
    let accounts = account_uses(&cfg.quota, &views, &paused_accounts(&paused), &[]);
    let notes = notes(&projects, &repos, &checks, &devices);
    let plan = build(&PlanInput {
        now_ms: now,
        mode: live.dispatcher.mode,
        devices: &devices,
        paused: &paused,
        projects: &projects,
        repos: &repos,
        tasks: &tasks,
        accounts: &accounts,
        models: &live.dispatcher.models(),
        pass: None,
        picked_by: "no orchestrator pass yet",
        notes: &notes,
    });
    Ok(Gathered { plan, tasks })
}

/// Every project, with `owner_active` filled from the feed and the session metadata.
pub fn project_views(
    store: &Store,
    device: Option<&str>,
    now: UnixMs,
) -> Result<Vec<ProjectView>, StoreError> {
    let sessions = sessions(store)?;
    let mut views = store.list_projects(device)?;
    for v in &mut views {
        v.owner_active = owner_active(v, &sessions, now);
    }
    Ok(views)
}

/// Each device's session metadata from the advisor feed.
fn sessions(store: &Store) -> Result<Vec<(DeviceId, SessionMeta)>, StoreError> {
    Ok(store
        .advisor_inputs()?
        .into_iter()
        .flat_map(|s| {
            let device = s.inputs.device;
            s.inputs
                .sessions
                .into_iter()
                .map(move |m| (device.clone(), m))
        })
        .collect())
}

/// Per-project lines: a check that has not run or did not finish, and repositories a
/// device could not add.
fn notes(
    projects: &[ProjectView],
    repos: &[StoredRepo],
    checks: &[BaseCheck],
    devices: &[DeviceView],
) -> Vec<String> {
    let mut out = Vec::new();
    for v in projects.iter().filter(|v| v.project.enabled) {
        let p = &v.project;
        if p.settings.check_command.is_empty() {
            continue;
        }
        let repo = repos
            .iter()
            .find(|r| r.device == p.device && r.repo.path == p.path);
        let Some(commit) = repo.and_then(|r| r.repo.base_commit.as_deref()) else {
            continue;
        };
        let check = checks
            .iter()
            .find(|c| c.project == p.id && c.base_commit == commit);
        match check {
            Some(c) if c.finished_ms.is_some() && c.passed.is_none() => out.push(format!(
                "{}: the check did not finish: {}",
                p.name,
                c.detail.clone().unwrap_or_default()
            )),
            Some(c) if c.finished_ms.is_some() => {}
            _ => out.push(format!(
                "{}: the check has not run on {}{}",
                p.name,
                short(commit),
                held_by(devices, &p.device)
            )),
        }
    }
    out.extend(skipped(projects, repos));
    out
}

/// ": lines: hold: ..." when the device's room keeps dispatcher jobs from starting.
fn held_by(devices: &[DeviceView], device: &str) -> String {
    devices
        .iter()
        .find(|d| d.info.id == device)
        .and_then(|d| d.room.as_ref())
        .filter(|r| r.state != LineState::Run || r.open_slots == 0)
        .map(|r| format!(": lines: {}", r.reason))
        .unwrap_or_default()
}

fn skipped(projects: &[ProjectView], repos: &[StoredRepo]) -> Vec<String> {
    let mut devices: Vec<&str> = repos.iter().map(|r| r.device.as_str()).collect();
    devices.dedup();
    devices
        .into_iter()
        .filter_map(|d| {
            let known = |path: &str| {
                projects
                    .iter()
                    .any(|v| v.project.device == d && v.project.path == path)
            };
            let n = repos
                .iter()
                .filter(|r| r.device == d && !known(&r.repo.path))
                .count();
            (n > 0).then(|| {
                format!(
                    "{n} repositories were not added: the device has {MAX_PROJECTS_PER_DEVICE} projects"
                )
            })
        })
        .collect()
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
