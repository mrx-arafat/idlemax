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
    let (ctx, paths) = ctx(dir.path());
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
            "systemctl --user enable --now idlemax-agent.service"
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
        err.contains("enable --now")
            && err.contains("boom")
            && err.contains("run the command again"),
        "{err}"
    );
}
