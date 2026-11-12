use super::*;
use crate::dispatch::inputs::RepoReport;
use crate::dispatch::project::{Project, ProjectSettings};
use crate::dispatch::task::TaskSource;
use crate::types::DeviceInfo;

const NOW: UnixMs = 1_800_000_000_000;

fn device(slots: u32) -> DeviceView {
    DeviceView {
        info: DeviceInfo {
            id: "mac".into(),
            capabilities: vec!["claude".into()],
            ai_accounts: vec!["max".into()],
            ..Default::default()
        },
        online: true,
        enabled: Some(true),
        room: Some(RoomDecision {
            state: LineState::Run,
            reason: "run: memory 70% used".into(),
            open_slots: slots,
            at_ms: NOW,
        }),
        ..Default::default()
    }
}

fn project(id: i64, name: &str, owner_active: bool) -> ProjectView {
    ProjectView {
        project: Project {
            id,
            name: name.into(),
            device: "mac".into(),
            path: format!("/code/{name}"),
            enabled: true,
            settings: ProjectSettings {
                off_limits: vec!["secrets".into()],
                ..Default::default()
            },
        },
        owner_active,
        ..Default::default()
    }
}

fn task(id: &str, project: i64) -> Task {
    Task {
        id: id.into(),
        project: format!("p{project}"),
        project_id: project,
        device: "mac".into(),
        source: TaskSource::Roadmap,
        title: format!("Do {id}"),
        done_check: Some("it works".into()),
        scope_paths: Vec::new(),
        needs_you: None,
    }
}

fn account(usable: bool) -> AccountUse {
    AccountUse {
        account: "max".into(),
        provider: Provider::Claude,
        usable,
        why: if usable {
            "usable".into()
        } else {
            "5-hour window is 72% used, stops at 60%".into()
        },
        ..Default::default()
    }
}

fn pick(task: &str) -> PassPick {
    PassPick {
        task: task.into(),
        account: "max".into(),
        model: "sonnet".into(),
        reason: format!("{task} is small"),
        scope_paths: vec!["src/lib.rs".into()],
    }
}

fn repo(name: &str, touched: &[&str], clipped: bool) -> StoredRepo {
    StoredRepo {
        device: "mac".into(),
        received_ms: NOW,
        repo: RepoReport {
            path: format!("/code/{name}"),
            touched: touched.iter().map(|t| t.to_string()).collect(),
            touched_clipped: clipped,
            ..Default::default()
        },
    }
}

struct World {
    mode: Mode,
    devices: Vec<DeviceView>,
    paused: Vec<ControlScope>,
    projects: Vec<ProjectView>,
    repos: Vec<StoredRepo>,
    tasks: Vec<Task>,
    accounts: Vec<AccountUse>,
    pass: Option<PassResult>,
    refused: Vec<(String, String)>,
}

impl World {
    fn new() -> Self {
        Self {
            mode: Mode::Propose,
            devices: vec![device(2)],
            paused: Vec::new(),
            projects: vec![project(1, "app", false)],
            repos: vec![repo("app", &[], false)],
            tasks: vec![task("a", 1), task("b", 1)],
            accounts: vec![account(true)],
            pass: None,
            refused: Vec::new(),
        }
    }

    fn with_pass(mut self, picks: &[&str]) -> Self {
        self.pass = Some(PassResult {
            picks: picks.iter().map(|t| pick(t)).collect(),
            seen: self.tasks.iter().map(|t| t.id.clone()).collect(),
            ..Default::default()
        });
        self
    }

    fn build(&self) -> Plan {
        let models = [(Provider::Claude, "sonnet".to_string())];
        build(&PlanInput {
            now_ms: NOW,
            mode: self.mode,
            devices: &self.devices,
            paused: &self.paused,
            projects: &self.projects,
            repos: &self.repos,
            tasks: &self.tasks,
            accounts: &self.accounts,
            models: &models,
            pass: self.pass.as_ref(),
            picked_by: "no orchestrator pass yet",
            notes: &[],
            refused: &self.refused,
        })
    }
}

#[test]
fn without_a_pass_nothing_is_picked_and_every_ready_task_is_not_judged() {
    let plan = World::new().build();
    let d = &plan.devices[0];
    assert!(d.picks.is_empty());
    assert_eq!(plan.not_judged.len(), 2);
    let empty = d.empty.as_ref().unwrap();
    assert_eq!(empty.count, 2);
    assert!(
        empty.reason.contains("not judged by the orchestrator"),
        "{}",
        empty.reason
    );
}

#[test]
fn a_pass_fills_the_slots_in_its_order() {
    let plan = World::new().with_pass(&["b", "a"]).build();
    let picks = &plan.devices[0].picks;
    let ids: Vec<&str> = picks.iter().map(|p| p.task.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
    let p = &picks[0];
    assert_eq!(
        (p.provider, p.account.as_str(), p.model.as_str(), p.by),
        (Provider::Claude, "max", "sonnet", PickedBy::Orchestrator)
    );
    assert_eq!(p.reason, "b is small");
    assert_eq!(p.scope_paths, ["src/lib.rs"]);
    assert_eq!(p.avoid_paths, ["secrets"]);
    assert!(plan.not_judged.is_empty());
    assert!(plan.devices[0].empty.is_none());
}

#[test]
fn a_scope_naming_a_file_the_project_lacks_becomes_the_whole_project() {
    // The live pass named ["demo.py", "tests/"] in a project without demo.py.
    let mut w = World::new().with_pass(&["a", "b"]);
    w.repos[0].repo.files = vec!["calc/__init__.py".into(), "tests/test_calc.py".into()];
    let picks = &mut w.pass.as_mut().unwrap().picks;
    picks[0].scope_paths = vec!["demo.py".into(), "tests/".into()];
    picks[1].scope_paths = vec!["*.py".into(), "tests/".into()];
    let plan = w.build();
    let (a, b) = (&plan.devices[0].picks[0], &plan.devices[0].picks[1]);
    assert_eq!(a.scope_paths, ["**"]);
    assert_eq!(
        a.reason,
        "a is small (scope widened: the named paths did not exist)"
    );
    assert_eq!(b.scope_paths, ["*.py", "tests/"]);
    assert_eq!(b.reason, "b is small");
}

#[test]
fn auto_mode_picks_like_propose() {
    let mut world = World::new().with_pass(&["b", "a"]);
    world.mode = Mode::Auto;
    let plan = world.build();
    assert_eq!(plan.mode, Mode::Auto);
    assert_eq!(plan.devices[0].picks.len(), 2);
}

#[test]
fn a_pick_on_an_account_that_may_not_be_spent_is_not_used() {
    let mut w = World::new().with_pass(&["a"]);
    w.accounts = vec![account(false)];
    let d = &w.build().devices[0];
    assert!(d.picks.is_empty());
    let reason = &d.empty.as_ref().unwrap().reason;
    assert!(
        reason.starts_with("Do a: account max may not be spent: 5-hour"),
        "{reason}"
    );

    let mut w = World::new().with_pass(&["a"]);
    w.devices[0].info.ai_accounts.clear();
    assert!(w.build().devices[0].picks.is_empty());
}

#[test]
fn the_owners_active_project_is_used_only_when_nothing_else_is_ready() {
    let mut w = World::new().with_pass(&["a"]);
    w.projects = vec![project(1, "app", true), project(2, "lib", false)];
    w.repos = vec![
        repo("app", &["src/main.rs"], false),
        repo("lib", &[], false),
    ];
    w.tasks = vec![task("a", 1), task("c", 2)];
    let w = w.with_pass(&["a"]);
    let d = &w.build().devices[0];
    assert!(d.picks.is_empty());

    let mut only = w;
    only.tasks = vec![task("a", 1)];
    let only = only.with_pass(&["a"]);
    let plan = only.build();
    assert_eq!(
        plan.devices[0].picks[0].avoid_paths,
        ["secrets", "src/main.rs"]
    );

    let mut clipped = only;
    clipped.repos[0].repo.touched_clipped = true;
    assert!(clipped.build().devices[0].picks.is_empty());
}

#[test]
fn a_held_device_and_mode_off_pick_nothing() {
    let mut hold = World::new().with_pass(&["a"]);
    hold.devices[0].room.as_mut().unwrap().state = LineState::Hold;
    let plan = hold.build();
    assert!(plan.devices[0].picks.is_empty());
    assert_eq!(plan.devices[0].room.state, LineState::Hold);

    let mut paused = World::new().with_pass(&["a"]);
    paused.paused = vec![ControlScope::Global];
    let plan = paused.build();
    assert!(plan.devices[0].picks.is_empty());
    assert_eq!(
        plan.devices[0].held.as_deref(),
        Some("paused by you: idlemax resume")
    );

    let mut off = World::new().with_pass(&["a"]);
    off.mode = Mode::Off;
    assert!(off.build().devices[0].picks.is_empty());

    let mut off_device = World::new().with_pass(&["a"]);
    off_device.devices[0].enabled = Some(false);
    let plan = off_device.build();
    assert!(plan.devices[0].picks.is_empty());
    assert_eq!(
        plan.devices[0].held.as_deref(),
        Some("device mac is off: idlemax device on mac")
    );

    let mut unspec_device = World::new().with_pass(&["a"]);
    unspec_device.devices[0].enabled = None;
    let plan = unspec_device.build();
    assert!(plan.devices[0].picks.is_empty());
    assert_eq!(
        plan.devices[0].held.as_deref(),
        Some("device mac is off: idlemax device on mac")
    );
}

#[test]
fn a_refused_pick_names_its_refusal_and_the_next_pick_fills_the_slot() {
    let mut w = World::new().with_pass(&["a", "b"]);
    w.devices = vec![device(1)];
    w.refused = vec![("a".into(), "needs 8.0%, 5.3% left today".into())];
    let plan = w.build();
    let ids: Vec<&str> = plan.devices[0]
        .picks
        .iter()
        .map(|p| p.task.as_str())
        .collect();
    assert_eq!(ids, ["b"]);

    w.tasks.truncate(1);
    let w = w.with_pass(&["a"]);
    let plan = w.build();
    assert!(plan.devices[0].picks.is_empty());
    assert_eq!(
        plan.devices[0].empty.as_ref().unwrap().reason,
        "Do a: needs 8.0%, 5.3% left today"
    );
}
