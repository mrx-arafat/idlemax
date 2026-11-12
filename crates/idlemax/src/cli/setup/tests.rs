use std::cell::RefCell;
use std::path::Path;

use idlemax_core::install::service::{Platform, ServiceEnv};

use super::steps::{execute, Env};
use super::*;
use crate::cli::service::{Output, Runner};

/// Records every command; nothing real is ever started.
struct Fake {
    seen: RefCell<Vec<String>>,
    fail_on: Option<&'static str>,
    /// What `systemctl show` answers.
    active: &'static str,
}

impl Runner for Fake {
    fn run(&self, step: &idlemax_core::install::service::Step) -> std::io::Result<Output> {
        let line = step.argv.join(" ");
        let fail = self.fail_on.is_some_and(|f| line.contains(f));
        let stdout = if line.contains(" show ") {
            format!("LoadState=loaded\nActiveState={}\n", self.active)
        } else {
            String::new()
        };
        self.seen.borrow_mut().push(line);
        Ok(Output {
            ok: !fail,
            stdout,
            stderr: if fail { "boom".into() } else { String::new() },
        })
    }
}

fn fake(fail_on: Option<&'static str>) -> Fake {
    Fake {
        seen: RefCell::default(),
        fail_on,
        active: "active",
    }
}

fn env<'a>(root: &Path, paths: &'a Paths, runner: &'a Fake, dry_run: bool) -> Env<'a> {
    let svc = Ctx {
        platform: Platform::SystemdUser,
        env: ServiceEnv {
            agent_jobs: 1,
            agent_ram_mib: 512,
            agent_tasks_max: 256,
            bin: "/opt/idlemax".into(),
            home: root.join("home"),
            state_dir: paths.root().to_path_buf(),
            path: Some("/usr/bin".into()),
            vars: vec![],
            uid: 501,
        },
    };
    Env {
        paths,
        svc: Some(svc),
        places: None,
        runner,
        dry_run,
        token: None,
        allow: None,
    }
}

fn plan_both(paths: &Paths) -> SetupPlan {
    let req = Request {
        role: Role::Both,
        os: Os::Linux,
        profile: None,
        coord_url: None,
        token_given: false,
        service: true,
        statusline: false,
    };
    plan_setup(paths, &req).unwrap().unwrap()
}

fn statuses(steps: &[StepReport]) -> Vec<Status> {
    steps.iter().map(|s| s.status).collect()
}

#[test]
fn an_empty_allowlist_is_the_first_next_step_after_setup() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let runner = fake(None);
    let plan = plan_both(&paths);
    let steps = execute(&plan, &env(dir.path(), &paths, &runner, false));
    let report = finish(&plan, false, steps, None, true);
    assert_eq!(
        report.next.first().map(String::as_str),
        Some("idlemax config set agent.command_allowlist git,cargo"),
        "{}",
        text::render(&report)
    );
    let again = execute(&plan_both(&paths), &env(dir.path(), &paths, &runner, false));
    let report = finish(&plan_both(&paths), false, again, None, true);
    let result = text::render(&report);
    assert!(result.contains("nothing to change"), "{result}");
    assert!(result.contains("idlemax config set agent.command_allowlist git,cargo"));
}

#[test]
fn allow_sets_the_list_in_setup_and_preserves_comments_and_repeated_runs() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    std::fs::create_dir_all(paths.root()).unwrap();
    let before = "# keep this heading\n[coordinator]\nlisten = \"127.0.0.1:18310\"\nurl = \"http://127.0.0.1:18310\"\n[agent] # my device\nlisten = \"127.0.0.1:18311\"\ncommand_allowlist = [] # only these programs\n";
    std::fs::write(paths.config_file(), before).unwrap();
    let runner = fake(None);
    let plan = plan_both(&paths);
    let (edit, needs_allowlist) = allowlist::prepare(&paths, &plan, Some("git,cargo")).unwrap();
    assert!(!needs_allowlist);
    let steps = execute(
        &plan,
        &Env {
            allow: edit.as_ref(),
            ..env(dir.path(), &paths, &runner, false)
        },
    );
    assert_eq!(
        steps.iter().find(|s| s.step == "allowlist").unwrap().status,
        Status::Done
    );
    let saved = std::fs::read_to_string(paths.config_file()).unwrap();
    assert_eq!(
        Config::parse(&saved).unwrap().agent.command_allowlist,
        ["git", "cargo"]
    );
    for comment in [
        "# keep this heading",
        "# my device",
        "# only these programs",
    ] {
        assert!(saved.contains(comment), "{saved}");
    }
    assert_eq!(
        std::fs::read_to_string(crate::cli::files::backup_path(&paths.config_file())).unwrap(),
        before
    );
    let (edit, _) = allowlist::prepare(&paths, &plan_both(&paths), Some("git,cargo")).unwrap();
    let again = execute(
        &plan_both(&paths),
        &Env {
            allow: edit.as_ref(),
            ..env(dir.path(), &paths, &runner, false)
        },
    );
    assert_eq!(
        again.iter().find(|s| s.step == "allowlist").unwrap().status,
        Status::AlreadyDone
    );
    assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), saved);
    println!("Saved config from setup steps with a fake service runner:\n{saved}");
}

#[test]
fn a_starting_setup_reports_doctor_as_the_next_step_without_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let report = finish(
        &plan_both(&paths),
        false,
        vec![StepReport::new(
            "doctor",
            Status::Starting,
            "services are still starting",
            "idlemax doctor".into(),
        )],
        None,
        false,
    );
    assert_eq!(report.next, ["idlemax doctor"]);
    assert!(report.doctor.is_none());
    let text = text::render(&report);
    assert!(text.contains("starting"));
    assert!(text.contains("run `idlemax doctor` to re-check"));
    assert!(!text.contains("warn"));
}

#[test]
fn a_dry_run_changes_nothing_and_a_second_run_changes_nothing_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let runner = fake(None);

    let dry = execute(&plan_both(&paths), &env(dir.path(), &paths, &runner, true));
    assert_eq!(
        statuses(&dry),
        [
            Status::WouldDo,
            Status::WouldDo,
            Status::WouldDo,
            Status::WouldDo,
            Status::Skipped
        ]
    );
    assert!(!paths.root().exists() && !dir.path().join("home").exists());
    assert!(runner.seen.borrow().is_empty());

    let first = execute(&plan_both(&paths), &env(dir.path(), &paths, &runner, false));
    assert_eq!(statuses(&first)[..4], [Status::Done; 4]);
    let ran = runner.seen.borrow().len();
    assert!(ran >= 4, "each service is enabled: {:?}", runner.seen);

    let second = execute(&plan_both(&paths), &env(dir.path(), &paths, &runner, false));
    assert_eq!(statuses(&second)[..4], [Status::AlreadyDone; 4]);
    let after: Vec<String> = runner.seen.borrow()[ran..].to_vec();
    assert!(
        after.iter().all(|c| c.contains(" show ")),
        "a current, running unit is only asked about: {after:?}"
    );
    assert!(!finish(&plan_both(&paths), false, second, None, false).changed);
}

#[test]
fn a_failing_service_command_stops_the_run_and_the_rest_is_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let runner = fake(Some("restart idlemax-coord"));

    let steps = execute(&plan_both(&paths), &env(dir.path(), &paths, &runner, false));
    assert_eq!(
        statuses(&steps),
        [
            Status::Done,
            Status::Done,
            Status::Failed,
            Status::Skipped,
            Status::Skipped
        ]
    );
    assert!(steps[2].detail.contains("boom"), "{}", steps[2].detail);
    assert!(
        steps[3].detail.contains("service step failed"),
        "{}",
        steps[3].detail
    );
    assert!(!finish(&plan_both(&paths), false, steps, None, false).ok);
}

#[test]
fn a_different_token_on_stdin_is_refused_and_the_same_one_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let runner = fake(None);
    let with_token = |token: &str| Env {
        token: Some(token.into()),
        svc: None,
        ..env(dir.path(), &paths, &runner, false)
    };
    let req = Request {
        role: Role::Agent,
        os: Os::Linux,
        profile: None,
        coord_url: Some("http://100.64.0.10:7878"),
        token_given: true,
        service: false,
        statusline: false,
    };
    let run = |token: &str| {
        let plan = plan_setup(&paths, &req).unwrap().unwrap();
        execute(&plan, &with_token(token))
    };
    assert_eq!(
        run("shared-secret-0123456789abcdef-0123")[1].status,
        Status::Done
    );
    assert_eq!(
        run("shared-secret-0123456789abcdef-0123")[1].status,
        Status::AlreadyDone
    );
    let other = run("another-secret-0123456789abcdef-012");
    assert_eq!(other[1].status, Status::Failed);
    assert!(
        other[1].detail.contains("different token"),
        "{}",
        other[1].detail
    );
    assert_eq!(
        std::fs::read_to_string(paths.token_file()).unwrap().trim(),
        "shared-secret-0123456789abcdef-0123"
    );
}

#[test]
fn a_current_unit_that_is_not_running_is_started_instead_of_reported_done() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("state"));
    let healthy = fake(None);
    execute(
        &plan_both(&paths),
        &env(dir.path(), &paths, &healthy, false),
    );

    // The first run wrote the files but the service never came up.
    let stopped = Fake {
        active: "inactive",
        ..fake(None)
    };
    let again = execute(
        &plan_both(&paths),
        &env(dir.path(), &paths, &stopped, false),
    );
    assert_eq!(
        statuses(&again)[2..4],
        [Status::Done; 2],
        "the two services"
    );
    let seen = stopped.seen.borrow();
    assert!(
        seen.iter()
            .any(|c| c == "systemctl --user start idlemax-agent.service"),
        "{seen:?}"
    );
}
