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
}

impl Runner for Fake {
    fn run(&self, step: &idlemax_core::install::service::Step) -> std::io::Result<Output> {
        let line = step.argv.join(" ");
        let fail = self.fail_on.is_some_and(|f| line.contains(f));
        self.seen.borrow_mut().push(line);
        Ok(Output {
            ok: !fail,
            stdout: String::new(),
            stderr: if fail { "boom".into() } else { String::new() },
        })
    }
}

fn fake(fail_on: Option<&'static str>) -> Fake {
    Fake {
        seen: RefCell::default(),
        fail_on,
    }
}

fn env<'a>(root: &Path, paths: &'a Paths, runner: &'a Fake, dry_run: bool) -> Env<'a> {
    let svc = Ctx {
        platform: Platform::SystemdUser,
        env: ServiceEnv {
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
    }
}

fn plan_both(paths: &Paths) -> SetupPlan {
    let req = Request {
        role: Role::Both,
        os: Os::Linux,
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
    assert_eq!(
        runner.seen.borrow().len(),
        ran,
        "a current unit is not reloaded"
    );
    assert!(!finish(&plan_both(&paths), false, second, None).changed);
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
    assert!(!finish(&plan_both(&paths), false, steps, None).ok);
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
