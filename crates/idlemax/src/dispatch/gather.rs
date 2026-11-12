//! Adapter: reads everything the plan needs under one store lock and builds it through the
//! pure rules (`discover`, `account_uses`, `build`). Writes nothing.

use idlemax_core::advice::{merge_inputs, SessionMeta};
use idlemax_core::dispatch::accounts::account_uses;
use idlemax_core::dispatch::accounts::AccountUse;
use idlemax_core::dispatch::check::BaseCheck;
use idlemax_core::dispatch::inputs::StoredRepo;
use idlemax_core::dispatch::orchestrator::{is_pass, pass_due, DueInput, Offer};
use idlemax_core::dispatch::plan::{build, offered, PassResult, Plan, PlanInput};
use idlemax_core::dispatch::project::{owner_active, ProjectView, MAX_PROJECTS_PER_DEVICE};
use idlemax_core::dispatch::room::LineState;
use idlemax_core::dispatch::task::{discover, Task, TaskInput};
use idlemax_core::dispatch::Mode;
use idlemax_core::protocol::DeviceView;
use idlemax_core::quota::identity::AccountProblem;
use idlemax_core::quota::pass::any_room;
use idlemax_core::quota::{tier_models, Provider, QuotaView};
use idlemax_core::types::{ControlScope, DeviceId, UnixMs};
use idlemax_core::Config;

use super::quota_side::QuotaSide;
use crate::coord::api::quota_views;
use crate::coord::store::{PassRow, Store, StoreError};

mod accounts;

use accounts::{open_accounts, OpenAccounts};

/// The Plan and every task `discover` returned.
#[derive(Debug, Clone)]
pub struct Gathered {
    pub plan: Plan,
    pub tasks: Vec<Task>,
    /// The quota views the plan was built from, so the dispatcher view need not read them
    /// again under the same lock.
    pub views: Vec<QuotaView>,
    /// What closes an account somewhere: the view lists it and a relaunch asks it.
    pub problems: Vec<AccountProblem>,
}

/// Everything the plan and the pass read, from one store lock.
pub struct Inputs {
    pub now: UnixMs,
    pub problems: Vec<AccountProblem>,
    pub mode: Mode,
    pub devices: Vec<DeviceView>,
    pub paused: Vec<ControlScope>,
    pub projects: Vec<ProjectView>,
    pub repos: Vec<StoredRepo>,
    pub tasks: Vec<Task>,
    pub accounts: Vec<AccountUse>,
    pub models: Vec<(Provider, String)>,
    pub notes: Vec<String>,
    /// The newest pass row, and the newest one with a result.
    pub newest: Option<PassRow>,
    pub newest_result: Option<PassRow>,
    /// A pass job is queued, leased, running or paused.
    pub pass_active: bool,
    /// Budgets, account offers, the cost table and failed checks (Spec 3).
    pub quota: QuotaSide,
    pub views: Vec<QuotaView>,
}

/// `cfg` is the coordinator's start config and gives `[quota]`; `live` is the newest
/// `config.toml` and gives `[dispatcher]`, so a mode change needs no restart.
pub fn read_inputs(
    store: &Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<Inputs, StoreError> {
    let OpenAccounts {
        devices,
        notes: closed,
        problems,
    } = open_accounts(store, cfg, now)?;
    let paused = store.paused_scopes()?;
    let projects = project_views(store, None, now)?;
    let repos = store.dispatch_inputs()?;
    let checks = store.base_checks()?;
    let newest_result = store.newest_pass(true)?;
    let scoped = scoped(newest_result.as_ref());
    let (github, _) = merge_inputs(&store.advisor_inputs()?, now);
    let tasks = discover(&TaskInput {
        now_ms: now,
        projects: &projects,
        repos: &repos,
        checks: &checks,
        issues: &github.assigned_issues,
        sessions: &sessions(store)?,
        judgments: &store.judgments()?,
        scoped: &scoped,
    });
    let views = quota_views(store, cfg, now)?;
    let quota = QuotaSide::read(store, cfg, live, &views, now)?;
    let accounts = account_uses(&quota.budgets, &views);
    let mut notes = notes(&projects, &repos, &checks, &devices);
    notes.extend(closed);
    // Discovery goes silent when Herdr is missing or renamed a key; say so (Risks table).
    let silent = store.devices_without_herdr()?.into_iter();
    notes.extend(silent.map(|d| format!("{d}: herdr gave no folders")));
    let pass_active = pass_active(store)?;
    Ok(Inputs {
        now,
        problems,
        mode: live.dispatcher.mode,
        devices,
        paused,
        projects,
        repos,
        tasks,
        accounts,
        models: tier_models(&live.quota),
        notes,
        newest: store.newest_pass(false)?,
        newest_result,
        pass_active,
        quota,
        views,
    })
}

/// (task id, scope paths) of the newest pass's picks.
fn scoped(newest: Option<&PassRow>) -> Vec<(String, Vec<String>)> {
    newest
        .iter()
        .flat_map(|r| r.result.iter().flat_map(|p| p.picks.iter()))
        .map(|p| (p.task.clone(), p.scope_paths.clone()))
        .collect()
}

/// A pass job is queued, leased, running or paused.
fn pass_active(store: &Store) -> Result<bool, StoreError> {
    Ok(store.active_jobs()?.iter().any(|j| is_pass(&j.spec))
        || store.queued_jobs()?.iter().any(|j| is_pass(&j.spec)))
}

impl Inputs {
    pub fn plan_input<'a>(
        &'a self,
        pass: Option<&'a PassResult>,
        picked_by: &'a str,
        refused: &'a [(String, String)],
    ) -> PlanInput<'a> {
        PlanInput {
            now_ms: self.now,
            mode: self.mode,
            devices: &self.devices,
            paused: &self.paused,
            projects: &self.projects,
            repos: &self.repos,
            tasks: &self.tasks,
            accounts: &self.accounts,
            models: &self.models,
            pass,
            picked_by,
            notes: &self.notes,
            refused,
        }
    }

    /// What a pass is offered: the ready tasks of every device, in plan order, the usable
    /// accounts and the models.
    pub fn offered(&self) -> (Vec<&Task>, Vec<AccountUse>) {
        let input = self.plan_input(None, "", &[]);
        let mut tasks: Vec<&Task> = Vec::new();
        for d in &self.devices {
            for t in offered(&input, &d.info.id) {
                if !tasks.iter().any(|x| x.id == t.id) {
                    tasks.push(t);
                }
            }
        }
        let usable = self.accounts.iter().filter(|a| a.usable).cloned().collect();
        (tasks, usable)
    }
}

/// The plan: the picks of the newest pass with a result, or why there is none.
pub fn gather_plan(
    store: &Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<Gathered, StoreError> {
    let inputs = read_inputs(store, cfg, live, now)?;
    let stored = inputs
        .newest_result
        .as_ref()
        .and_then(|r| r.result.as_ref());
    let (tasks, _) = inputs.offered();
    let checked = stored.map(|p| inputs.quota.recheck(p, &tasks));
    let (pass, refused) = match &checked {
        Some((p, r)) => (Some(p), r.as_slice()),
        None => (None, &[][..]),
    };
    let picked_by = picked_by(&inputs);
    let plan = build(&inputs.plan_input(pass, &picked_by, refused));
    Ok(Gathered {
        plan,
        tasks: inputs.tasks,
        views: inputs.views,
        problems: inputs.problems,
    })
}

/// "orchestrator pass 12m ago", or why no pass has a result.
fn picked_by(inputs: &Inputs) -> String {
    if let Some(row) = &inputs.newest_result {
        let mut text = format!("orchestrator pass {} ago", age(inputs.now - row.queued_ms));
        let dropped = row.result.as_ref().map_or(0, |r| r.dropped.len());
        if dropped > 0 {
            text.push_str(&format!(", {dropped} picks dropped"));
        }
        return text;
    }
    if inputs.pass_active {
        return "no orchestrator pass yet: one is queued or running".into();
    }
    if let Some(error) = inputs.newest.as_ref().and_then(|r| r.error.clone()) {
        return format!("no orchestrator pass: the last pass failed: {error}");
    }
    let (tasks, accounts) = inputs.offered();
    let quota = inputs.quota.offer();
    // Whether a pass is due never depends on the finished runs.
    let offer = Offer {
        tasks: &tasks,
        accounts: &accounts,
        models: &inputs.models,
        quota: Some(&quota),
        finished: &[],
    };
    let due = pass_due(&DueInput {
        now_ms: inputs.now,
        mode: inputs.mode,
        asked: true,
        devices: &inputs.devices,
        paused: &inputs.paused,
        offer: &offer,
        fingerprint: "",
        newest: None,
        pass_active: false,
        room: any_room(&inputs.quota.offers),
    });
    match due {
        Ok(_) => "no orchestrator pass yet: one is due".into(),
        Err(why) => format!("no orchestrator pass: {why}"),
    }
}

/// "42m", "3h 5m", "2d 4h".
fn age(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    match (mins / 1440, mins % 1440 / 60, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
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
