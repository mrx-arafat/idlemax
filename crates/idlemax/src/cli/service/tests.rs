use std::cell::RefCell;

use super::*;

/// Records every command and answers from a script; nothing real is ever started.
struct Fake {
    seen: RefCell<Vec<String>>,
    fail_on: Option<&'static str>,
    show: &'static str,
}

impl Runner for Fake {
    fn run(&self, step: &Step) -> std::io::Result<Output> {
        let line = step.to_string();
        let fail = self.fail_on.is_some_and(|f| line.contains(f));
        self.seen.borrow_mut().push(step.argv.join(" "));
        Ok(Output {
            ok: !fail,
            stdout: self.show.into(),
            stderr: if fail {
                "boom\nmore".into()
            } else {
                String::new()
            },
        })
    }
}

fn fake(fail_on: Option<&'static str>) -> Fake {
    Fake {
        seen: RefCell::default(),
        fail_on,
        show: "LoadState=loaded\nActiveState=active\n",
    }
}

fn ctx(root: &Path) -> (Ctx, Paths) {
    let paths = Paths::new(root.join("state"));
    let env = ServiceEnv {
        agent_jobs: 1,
        agent_ram_mib: 512,
        agent_tasks_max: 256,
        bin: "/opt/idlemax".into(),
        home: root.join("home"),
        state_dir: paths.root().to_path_buf(),
        path: Some("/usr/bin".into()),
        vars: vec![],
        uid: 501,
    };
    (
        Ctx {
            platform: Platform::SystemdUser,
            env,
        },
        paths,
    )
}

#[test]
fn install_status_uninstall_round_trip_under_a_temp_home() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ctx, paths) = ctx(dir.path());
    ctx.env.agent_ram_mib = 4096;
    ctx.env.agent_jobs = 15;
    let runner = fake(None);
    let all = [ServiceKind::Agent, ServiceKind::Coord];

    let before = status(&ctx, &runner, &all);
    assert!(before.iter().all(|s| !s.installed));
    assert!(
        runner.seen.borrow().is_empty(),
        "asked the service manager about an uninstalled unit"
    );

    let dry = install(&ctx, &paths, &runner, &[ServiceKind::Agent], true).unwrap();
    assert!(!dry[0].file.exists() && runner.seen.borrow().is_empty());
    assert!(dry[0].content.contains("MemoryMax=4224M"));
    assert!(dry[0].content.contains("CPUQuota=1500%"));

    let done = install(&ctx, &paths, &runner, &[ServiceKind::Agent], false).unwrap();
    let written = std::fs::read_to_string(&done[0].file).unwrap();
    assert_eq!(written, done[0].content);
    assert!(done[0]
        .file
        .starts_with(dir.path().join("home/.config/systemd/user")));
    assert!(
        done[0].notes.iter().any(|n| n.contains("no token")),
        "{:?}",
        done[0].notes
    );
    assert_eq!(
        *runner.seen.borrow(),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable idlemax-agent.service",
            "systemctl --user restart idlemax-agent.service"
        ]
    );

    let now = status(&ctx, &runner, &all);
    assert_eq!(
        (now[0].installed, now[0].loaded, now[0].running),
        (true, Some(true), Some(true))
    );
    assert!(!now[1].installed);

    let gone = uninstall(&ctx, &runner, &all, false).unwrap();
    assert!(gone[0].installed && !gone[1].installed);
    assert!(!gone[0].file.exists());
    let seen = runner.seen.borrow();
    assert_eq!(
        seen[seen.len() - 2],
        "systemctl --user disable --now idlemax-agent.service"
    );
    assert_eq!(seen[seen.len() - 1], "systemctl --user daemon-reload");
}

#[test]
fn a_failed_step_says_what_failed_and_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, paths) = ctx(dir.path());
    let err = install(
        &ctx,
        &paths,
        &fake(Some("enable")),
        &[ServiceKind::Agent],
        false,
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        err.contains("systemctl --user enable")
            && err.contains("boom")
            && err.contains("run the command again"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn install_creates_a_missing_state_dir_and_logs_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (mut ctx, paths) = ctx(dir.path());
    ctx.platform = Platform::Launchd;
    install(&ctx, &paths, &fake(None), &[ServiceKind::Agent], false).unwrap();
    for made in [paths.root().to_path_buf(), paths.logs_dir()] {
        let mode = std::fs::metadata(&made).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", made.display());
    }
}

#[test]
fn installing_again_restarts_a_changed_unit_and_only_starts_an_unchanged_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ctx, paths) = ctx(dir.path());
    let runner = fake(None);
    let agent = [ServiceKind::Agent];
    let ran = |runner: &Fake| std::mem::take(&mut *runner.seen.borrow_mut());

    install(&ctx, &paths, &runner, &agent, false).unwrap();
    ran(&runner);
    install(&ctx, &paths, &runner, &agent, false).unwrap();
    assert_eq!(
        ran(&runner),
        [
            "systemctl --user enable idlemax-agent.service",
            "systemctl --user start idlemax-agent.service"
        ],
        "a running agent with a current unit is left alone"
    );

    ctx.env.path = Some("/usr/bin:/opt/other/bin".into());
    install(&ctx, &paths, &runner, &agent, false).unwrap();
    let steps = ran(&runner);
    assert_eq!(
        steps.last().map(String::as_str),
        Some("systemctl --user restart idlemax-agent.service"),
        "{steps:?}"
    );
}

/// An installed agent unit in a temp home.
fn installed() -> (Ctx, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, paths) = ctx(dir.path());
    let done = install(&ctx, &paths, &fake(None), &[ServiceKind::Agent], false).unwrap();
    let file = done[0].file.clone();
    (ctx, dir, file)
}

#[test]
fn uninstall_keeps_the_file_when_a_loaded_service_cannot_be_stopped() {
    let runner = fake(Some("disable --now"));
    let (ctx, _dir, file) = installed();
    let err = uninstall(&ctx, &runner, &[ServiceKind::Agent], false)
        .err()
        .expect("a failed stop must be an error")
        .to_string();
    assert!(file.exists(), "the service would be left running unmanaged");
    assert!(
        err.contains("disable --now") && err.contains("in place"),
        "{err}"
    );
}

#[test]
fn uninstall_skips_the_stop_for_a_service_that_is_not_loaded() {
    let runner = Fake {
        show: "LoadState=not-found\nActiveState=inactive\n",
        ..fake(Some("disable --now"))
    };
    let (ctx, _dir, file) = installed();
    let gone = uninstall(&ctx, &runner, &[ServiceKind::Agent], false).unwrap();
    assert!(gone[0].installed && !file.exists());
    let seen = runner.seen.borrow();
    assert!(!seen.iter().any(|c| c.contains("disable")), "{seen:?}");
}

#[test]
fn a_failure_after_the_file_is_deleted_says_it_is_gone() {
    let runner = fake(Some("daemon-reload"));
    let (ctx, _dir, file) = installed();
    let err = uninstall(&ctx, &runner, &[ServiceKind::Agent], false)
        .err()
        .expect("the reload failed")
        .to_string();
    assert!(!file.exists());
    assert!(err.contains("already deleted"), "{err}");
}
