//! The worker lock through the real binaries: a coordinator and an agent in a temp home refuse
//! an owner-only call whose caller has an ancestor that idlemax launched for this home, the
//! CLI refuses one from its own marked environment before any call, and stop calls still pass.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const LAUNCHED: &str = "this call comes from a job or worker that idlemax started";

/// A daemon that is killed on drop and dumps its log if the test failed.
struct Proc {
    child: Child,
    log: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("{}", std::fs::read_to_string(&self.log).unwrap_or_default());
        }
    }
}

/// A coordinator and an agent in a temp home on spare ports.
struct Home {
    dir: tempfile::TempDir,
    coord: String,
    agent: String,
    token: String,
    _daemons: Vec<Proc>,
}

impl Home {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        assert!(state.starts_with(std::env::temp_dir()));
        let (coord_port, agent_port) = (free_port(), free_port());
        let coord = format!("http://127.0.0.1:{coord_port}");
        std::fs::write(
            state.join("config.toml"),
            format!(
                "[coordinator]\nlisten = \"127.0.0.1:{coord_port}\"\nurl = \"{coord}\"\n\n\
                 [agent]\ndevice_id = \"lock\"\nlisten = \"127.0.0.1:{agent_port}\"\n\
                 advisor_inputs_secs = 0\n\n[advisor]\nenabled = false\n"
            ),
        )
        .unwrap();
        let mut home = Self {
            dir,
            coord,
            agent: format!("http://127.0.0.1:{agent_port}"),
            token: String::new(),
            _daemons: Vec::new(),
        };
        assert!(home.cmd(&["init"]).status().unwrap().success());
        home.token = std::fs::read_to_string(state.join("token"))
            .unwrap()
            .trim()
            .to_string();
        home._daemons = vec![home.daemon("coord"), home.daemon("agent")];
        wait_up(&format!("{}/healthz", home.coord));
        wait_up(&format!("{}/healthz", home.agent));
        home
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    /// `idlemax <args>` in this home with a clean environment.
    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", self.dir.path().join("home"))
            .env("IDLEMAX_HOME", self.state())
            .env_remove("IDLEMAX_TOKEN")
            .env_remove("IDLEMAX_JOB_ID");
        cmd
    }

    fn daemon(&self, name: &str) -> Proc {
        let log = self.dir.path().join(format!("{name}.log"));
        let out = File::create(&log).unwrap();
        let child = self
            .cmd(&[name])
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        Proc { child, log }
    }

    /// Runs `inner` with a clean environment under the job supervisor, whose environment
    /// carries the marker when `marker` is set: `Some(None)` names no home, `Some(Some(h))`
    /// names `h`. Only the parent of the calling process is marked.
    fn under_job(&self, marker: Option<Option<&Path>>, inner: &[&str]) -> Output {
        let mut cmd = self.cmd(&["job-wrap", "--marker"]);
        cmd.arg(self.dir.path().join("job.exit"))
            .args(["--", "/usr/bin/env", "-u", "IDLEMAX_JOB_ID"])
            .arg(format!("IDLEMAX_HOME={}", self.state().display()))
            .args(inner);
        match marker {
            None => {}
            Some(home) => {
                cmd.env("IDLEMAX_JOB_ID", "7");
                match home {
                    Some(h) => cmd.env("IDLEMAX_HOME", h),
                    None => cmd.env_remove("IDLEMAX_HOME"),
                };
            }
        }
        cmd.output().unwrap()
    }

    /// `POST <base><path>` with `body` from under a job marked for this home: the status.
    fn post_marked(&self, base: &str, path: &str, body: &str) -> String {
        let auth = format!("Authorization: Bearer {}", self.token);
        let url = format!("{base}{path}");
        let curl = [
            "curl",
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-X",
            "POST",
            "-H",
            &auth,
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
            &url,
        ];
        let out = self.under_job(Some(Some(&self.state())), &curl);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_up(url: &str) {
    let start = Instant::now();
    while Command::new("curl")
        .args(["-sf", "-o", "/dev/null", url])
        .status()
        .is_ok_and(|s| !s.success())
    {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{url} never came up"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_launched_process_cannot_switch_idlemax_on_and_the_owner_can() {
    let home = Home::start();
    let resume = [BIN, "--coord", &home.coord, "resume"];
    // A resume with nothing paused is a no-op that never calls the coordinator.
    let pause = || assert!(home.cmd(&["pause"]).output().unwrap().status.success());

    pause();
    let refused = home.under_job(Some(Some(&home.state())), &resume);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(text(&refused).contains(LAUNCHED), "{}", text(&refused));

    let owner = home.under_job(None, &resume);
    assert_eq!(owner.status.code(), Some(0), "{}", text(&owner));

    pause();
    let other = tempfile::tempdir().unwrap();
    let other_home = home.under_job(Some(Some(other.path())), &resume);
    assert_eq!(other_home.status.code(), Some(0), "{}", text(&other_home));

    pause();
    let no_home = home.under_job(Some(None), &resume);
    assert_eq!(no_home.status.code(), Some(1), "{}", text(&no_home));
    assert!(text(&no_home).contains(LAUNCHED), "{}", text(&no_home));
}

#[test]
fn owner_only_routes_are_403_from_a_launched_process_and_stop_routes_are_not() {
    let home = Home::start();
    let coord = &home.coord;
    for (path, body) in [
        (
            "/v1/control",
            r#"{"action":"resume","scope":{"type":"global"}}"#,
        ),
        ("/v1/dispatcher/mode", r#"{"mode":"auto"}"#),
        ("/v1/dispatcher/mode", r#"{"mode":"propose"}"#),
        ("/v1/projects", r#"{"path":"/tmp/x"}"#),
        ("/v1/projects/1/on", "{}"),
        ("/v1/projects/1/settings", "{}"),
        (
            "/v1/limits",
            r#"{"key":"dispatcher.hold_mem_used_pct","value":"85"}"#,
        ),
        ("/v1/plan/refresh", ""),
        ("/v1/proposals/1/approve", ""),
        ("/v1/proposals/1/reject", ""),
    ] {
        assert_eq!(home.post_marked(coord, path, body), "403", "{path} {body}");
    }
    assert_eq!(
        home.post_marked(coord, "/v1/dispatcher/mode", r#"{"mode":"off"}"#),
        "200"
    );
    let stop = r#"{"action":"stop_all","scope":{"type":"global"}}"#;
    assert_eq!(home.post_marked(coord, "/v1/control", stop), "200");
    assert_eq!(
        home.post_marked(coord, "/v1/projects/999999/off", "{}"),
        "404"
    );

    assert_eq!(home.post_marked(&home.agent, "/v1/local/reload", ""), "403");
    let pause = r#"{"action":"pause","scope":{"type":"global"}}"#;
    assert_eq!(
        home.post_marked(&home.agent, "/v1/local/control", pause),
        "200"
    );
}

#[test]
fn the_cli_refuses_owner_commands_from_a_marked_environment_before_any_call() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let closed = format!("http://127.0.0.1:{}", free_port());
    let run = |args: &[&str], token: Option<&str>| {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", dir.path())
            .env("IDLEMAX_HOME", &state)
            .env("IDLEMAX_JOB_ID", "7");
        match token {
            Some(t) => cmd.env("IDLEMAX_TOKEN", t),
            None => cmd.env_remove("IDLEMAX_TOKEN"),
        };
        cmd.output().unwrap()
    };
    assert!(run(&["init"], None).status.success());
    let config = std::fs::read_to_string(state.join("config.toml")).unwrap();
    for (args, token) in [
        (&["--coord", &closed, "resume"][..], None),
        (&["--coord", &closed, "resume"][..], Some("")),
        (&["config", "set", "agent.notify", "true"][..], None),
    ] {
        let out = run(args, token);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", text(&out));
        assert!(text(&out).contains(LAUNCHED), "{args:?}: {}", text(&out));
        assert!(text(&out).contains("ask the owner"), "{}", text(&out));
    }
    let after = std::fs::read_to_string(state.join("config.toml")).unwrap();
    assert_eq!(after, config, "a refused config set changed the file");
}
