//! End to end: `idlemax setup` under a temp HOME. It never installs a service here: the cases
//! use --dry-run or --no-service, so launchctl and systemctl are never run.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

struct Home {
    _dir: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let state = dir.path().join("state");
        // Never run against the real user state (docs/PLAN.md rule 5).
        assert!(state.starts_with(std::env::temp_dir()));
        Home {
            home,
            state,
            _dir: dir,
        }
    }

    fn setup(&self, args: &[&str]) -> Output {
        // The tests pin the human report unless they ask for JSON.
        let format: &[&str] = if args.contains(&"--json") {
            &[]
        } else {
            &["--format", "table"]
        };
        Command::new(BIN)
            .args(format)
            .arg("setup")
            .args(args)
            .env("HOME", &self.home)
            .env("IDLEMAX_HOME", &self.state)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("IDLEMAX_TOKEN")
            .env_remove("IDLEMAX_COORD")
            .output()
            .unwrap()
    }

    /// Runs `setup ... --json` and returns (exit code, report).
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.setup(&all);
        let report = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (out.status.code().unwrap(), report)
    }
}

/// A port nothing listens on, so doctor's coordinator check cannot reach anything real.
fn dead_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn step<'a>(report: &'a Value, name: &str, which: Option<&str>) -> &'a Value {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == name && which.is_none_or(|w| s["which"] == w))
        .unwrap_or_else(|| panic!("no {name} step in {report:#}"))
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).unwrap().next().is_none()
}

#[test]
fn dry_run_lists_every_file_and_command_and_writes_nothing() {
    let h = Home::new();
    let (code, report) = h.json(&["--dry-run", "--statusline"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        (&report["dry_run"], &report["changed"]),
        (&true.into(), &false.into())
    );

    for (name, which) in [
        ("config", None),
        ("token", None),
        ("service", Some("coord")),
        ("service", Some("agent")),
        ("statusline", None),
        ("doctor", None),
    ] {
        let s = step(&report, name, which);
        assert_eq!(s["status"], "would_do", "{name}: {s:#}");
        assert!(
            s["command"].as_str().unwrap().starts_with("idlemax "),
            "{s:#}"
        );
    }
    let agent = step(&report, "service", Some("agent"));
    assert!(!agent["runs"].as_array().unwrap().is_empty(), "{agent:#}");
    let unit = agent["files"][0].as_str().unwrap();
    assert!(unit.starts_with(h.home.to_str().unwrap()), "{unit}");

    assert!(!h.state.exists(), "a dry run created the state dir");
    assert!(is_empty_dir(&h.home), "a dry run wrote under HOME");

    // Piped, the same plan is TOON: steps as rows, the files and commands as lists.
    let out = Command::new(BIN)
        .args(["setup", "--dry-run"])
        .env("HOME", &h.home)
        .env("IDLEMAX_HOME", &h.state)
        .env_remove("IDLEMAX_TOKEN")
        .env_remove("IDLEMAX_COORD")
        .output()
        .unwrap();
    let toon = String::from_utf8_lossy(&out.stdout);
    assert!(
        toon.starts_with("role: both\ndry_run: true\nok: true\n"),
        "{toon}"
    );
    assert!(
        toon.contains("steps[6]{step,status,detail,by_hand}:"),
        "{toon}"
    );
    assert!(
        toon.contains("would_write[") && toon.contains("would_run["),
        "{toon}"
    );
    assert!(toon.contains("agent.command_allowlist is empty"), "{toon}");
    assert!(
        toon.contains("Run `idlemax config set agent.command_allowlist git,cargo` to continue"),
        "{toon}"
    );
    assert!(is_empty_dir(&h.home), "the TOON dry run wrote under HOME");
}

#[test]
fn allow_flag_previews_the_allowlist_without_writing_it() {
    let home = Home::new();
    let (code, report) = home.json(&["--dry-run", "--allow", "git,cargo"]);
    assert_eq!(code, 0, "{report}");
    let allow = step(&report, "allowlist", None);
    assert_eq!(allow["status"], "would_do");
    assert!(allow["detail"].as_str().unwrap().contains("git"));
    assert!(allow["detail"].as_str().unwrap().contains("cargo"));
    assert!(!home.state.exists());
    assert!(is_empty_dir(&home.home));
    let (code, error) = home.json(&["--dry-run", "--allow", "[1]"]);
    assert_eq!(code, 2);
    assert_eq!(error["help"], "idlemax setup --help");
    assert!(
        !home.state.exists(),
        "invalid allowlist must be refused before writing"
    );
}

#[test]
fn a_second_run_changes_nothing_and_says_so() {
    let h = Home::new();
    let url = format!("http://127.0.0.1:{}", dead_port());
    let args = ["--no-service", "--statusline", "--coord-url", url.as_str()];

    let (code, first) = h.json(&args);
    assert_eq!(
        code, 0,
        "no service was asked for, so doctor's findings do not fail it"
    );
    assert_eq!(first["changed"], true);
    for name in ["config", "token", "statusline"] {
        assert_eq!(step(&first, name, None)["status"], "done", "{name}");
    }
    assert_eq!(step(&first, "service", None)["status"], "skipped");
    assert!(first["doctor"]["version"].is_string(), "doctor ran");

    let config = std::fs::read_to_string(h.state.join("config.toml")).unwrap();
    let port = url.rsplit(':').next().unwrap();
    assert!(config.contains(&format!("url = \"{url}\"")), "{config}");
    assert!(
        config.contains(&format!("listen = \"127.0.0.1:{port}\"")),
        "{config}"
    );
    let settings = std::fs::read_to_string(h.home.join(".claude/settings.json")).unwrap();
    assert!(
        settings.contains("claude-statusline-capture.sh"),
        "{settings}"
    );
    let token = std::fs::read_to_string(h.state.join("token")).unwrap();

    let (code, second) = h.json(&args);
    assert_eq!(code, 0);
    assert_eq!(second["changed"], false, "{second:#}");
    for name in ["config", "token", "statusline"] {
        assert_eq!(
            step(&second, name, None)["status"],
            "already_done",
            "{name}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(h.state.join("token")).unwrap(),
        token
    );

    let text = h.setup(&["--no-service", "--statusline", "--coord-url", url.as_str()]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("nothing to change"), "{text}");
}

#[test]
fn an_agent_without_the_coordinator_address_is_a_usage_error() {
    let h = Home::new();
    let out = h.setup(&["--role", "agent", "--no-service"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--coord-url"), "{err}");
    assert!(
        !h.state.exists(),
        "nothing is written when the flags cannot work"
    );
}

#[test]
fn every_setup_usage_error_points_to_setup_help_and_preserves_the_message() {
    for args in [
        vec!["--coord-url", "100.64.0.10:7878"],
        vec!["--role", "agent"],
        vec!["--role", "agent", "--coord-url", "http://100.64.0.10:7878"],
        vec!["--coord-url", "http://myhost:7878"],
    ] {
        let home = Home::new();
        let mut flags = vec!["--dry-run"];
        flags.extend(args);
        let (code, error) = home.json(&flags);
        assert_eq!(code, 2, "{error}");
        assert_eq!(error["help"], "idlemax setup --help", "{error}");
        assert!(
            error["error"].as_str().unwrap().contains("coord"),
            "{error}"
        );
    }
    let home = Home::new();
    std::fs::create_dir(&home.state).unwrap();
    std::fs::write(
        home.state.join("config.toml"),
        "[coordinator]\nurl = \"http://127.0.0.1:18310\"\n",
    )
    .unwrap();
    let (code, error) = home.json(&["--dry-run", "--coord-url", "http://100.64.0.10:7878"]);
    assert_eq!(code, 2);
    assert_eq!(error["help"], "idlemax setup --help");
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("already sets coordinator.url"));
}
