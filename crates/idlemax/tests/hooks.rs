//! End to end: `idlemax hooks` against Claude Code, Codex and OpenCode config files that live
//! under a temp HOME. Nothing here may touch the real ~/.claude, ~/.codex or ~/.config/opencode.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

const SETTINGS: &str = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"SessionStart\": [\n      {\"matcher\": \"startup\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo mine\"}]}\n    ]\n  }\n}\n";
const CODEX_CONFIG: &str = "model = \"o3\"\n";

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    /// PATH for the child: no idlemax on it unless a test adds this binary's directory.
    path: String,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), SETTINGS).unwrap();
        std::fs::write(home.join(".codex/config.toml"), CODEX_CONFIG).unwrap();
        assert!(
            home.starts_with(std::env::temp_dir()),
            "sandbox must be a temp dir"
        );
        Self {
            _tmp: tmp,
            home,
            path: "/usr/bin:/bin".into(),
        }
    }

    /// Runs `idlemax hooks <args> --json`; returns the exit code and the parsed report.
    fn hooks(&self, args: &[&str]) -> (i32, Value) {
        let out = Command::new(BIN)
            .arg("--json")
            .arg("hooks")
            .args(args)
            .env("HOME", &self.home)
            .env("IDLEMAX_HOME", self.home.join("state"))
            .env("PATH", &self.path)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let json = serde_json::from_str(&text).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {text}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap_or(-1), json)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.home.join(rel)).unwrap_or_default()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn outcome(report: &Value, agent: &str) -> String {
    let found = report["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == agent);
    found.map_or("absent".into(), |a| {
        a["outcome"].as_str().unwrap().to_string()
    })
}

/// Our hook commands in a settings or hooks file; none when the file has no SessionStart hooks.
fn our_commands(settings: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(settings).unwrap();
    let groups = v["hooks"]["SessionStart"].as_array().into_iter().flatten();
    groups
        .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
        .filter_map(|h| h["command"].as_str())
        .filter(|c| c.ends_with("hooks summary"))
        .map(String::from)
        .collect()
}

fn exe_command() -> String {
    let exe = std::fs::canonicalize(BIN).unwrap();
    idlemax_core::install::hooks::hook_command(exe.to_str().unwrap())
}

#[test]
fn hooks_install_repair_and_remove_only_their_own_entry() {
    let sb = Sandbox::new();

    // A dry run reports what would change and writes nothing; opencode is named as skipped.
    let (code, report) = sb.hooks(&["install", "--dry-run"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        (outcome(&report, "claude"), outcome(&report, "codex")),
        ("installed".into(), "installed".into())
    );
    assert_eq!(outcome(&report, "opencode"), "absent");
    assert_eq!(report["skipped"], serde_json::json!(["opencode"]));
    assert_eq!(sb.read(".claude/settings.json"), SETTINGS);
    assert!(!sb.home.join(".codex/hooks.json").exists());

    // Install: the user's own hook stays, ours is added, the old file is backed up.
    let (code, report) = sb.hooks(&["install"]);
    assert_eq!(code, 0, "{report}");
    let settings = sb.read(".claude/settings.json");
    assert_eq!(our_commands(&settings), [exe_command()]);
    assert!(settings.contains("echo mine") && settings.contains("\"model\": \"opus\""));
    assert_eq!(sb.read(".claude/settings.json.idlemax.bak"), SETTINGS);
    assert_eq!(our_commands(&sb.read(".codex/hooks.json")), [exe_command()]);
    assert_eq!(
        sb.read(".codex/config.toml"),
        "model = \"o3\"\n\n[features]\nhooks = true\n"
    );

    // Running it again is a silent no-op, byte for byte.
    let (code, report) = sb.hooks(&["install"]);
    assert_eq!(code, 0);
    assert_eq!(
        (outcome(&report, "claude"), outcome(&report, "codex")),
        ("unchanged".into(), "unchanged".into())
    );
    assert_eq!(sb.read(".claude/settings.json"), settings);

    // A moved binary is repaired: with this binary's directory on PATH the bare name is used.
    let dir = Path::new(BIN).parent().unwrap().display();
    let sb_path = format!("{dir}:/usr/bin:/bin");
    let mut sb = sb;
    sb.path = sb_path;
    let (_, report) = sb.hooks(&["install"]);
    assert_eq!(outcome(&report, "claude"), "repaired", "{report}");
    assert_eq!(
        our_commands(&sb.read(".claude/settings.json")),
        ["idlemax hooks summary"]
    );

    // A hook whose binary is gone shows as stale, and install fixes it.
    sb.write(
        ".codex/hooks.json",
        &sb.read(".codex/hooks.json")
            .replace("idlemax hooks summary", "/old/place/idlemax hooks summary"),
    );
    let (_, report) = sb.hooks(&["status"]);
    assert_eq!(
        (outcome(&report, "claude"), outcome(&report, "codex")),
        ("installed".into(), "stale".into()),
        "{report}"
    );
    assert_eq!(outcome(&report, "opencode"), "not_found");
    sb.hooks(&["install"]);
    assert_eq!(outcome(&sb.hooks(&["status"]).1, "codex"), "installed");

    // OpenCode is only set up when named, and a file that is not ours is never overwritten.
    let (_, report) = sb.hooks(&["install", "--opencode"]);
    assert_eq!(outcome(&report, "opencode"), "installed", "{report}");
    let plugin = sb.read(".config/opencode/plugins/idlemax.js");
    assert!(
        plugin.starts_with("// idlemax managed opencode plugin"),
        "{plugin}"
    );
    sb.hooks(&["uninstall", "--opencode"]);
    assert!(!sb.home.join(".config/opencode/plugins/idlemax.js").exists());
    sb.write(
        ".config/opencode/plugins/idlemax.js",
        "export const Mine = async () => ({});\n",
    );
    let (code, report) = sb.hooks(&["install", "--opencode"]);
    assert_eq!((code, outcome(&report, "opencode")), (1, "refused".into()));
    assert_eq!(
        sb.read(".config/opencode/plugins/idlemax.js"),
        "export const Mine = async () => ({});\n"
    );

    // A broken file is refused and left alone, and the other agent is still handled.
    sb.write(".claude/settings.json", "{ not json");
    let (code, report) = sb.hooks(&["install", "--claude", "--codex"]);
    assert_eq!(
        (code, outcome(&report, "claude"), outcome(&report, "codex")),
        (1, "refused".into(), "unchanged".into()),
        "{report}"
    );
    assert_eq!(sb.read(".claude/settings.json"), "{ not json");
    sb.write(
        ".claude/settings.json",
        &sb.read(".claude/settings.json.idlemax.bak"),
    );

    // Uninstall takes only our entry out: the user's settings come back exactly.
    let (code, report) = sb.hooks(&["uninstall", "--claude", "--codex"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(outcome(&report, "claude"), "removed", "{report}");
    assert_eq!(
        sb.read(".claude/settings.json"),
        SETTINGS,
        "the user's file comes back exactly"
    );
    assert!(our_commands(&sb.read(".codex/hooks.json")).is_empty());
    assert!(
        sb.read(".codex/config.toml").contains("hooks = true"),
        "the feature switch stays on"
    );
    assert_eq!(
        outcome(&sb.hooks(&["uninstall", "--claude"]).1, "claude"),
        "not_installed"
    );
}

#[test]
fn with_no_agent_installed_the_error_names_where_it_looked_and_what_to_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .args(["hooks", "install"])
        .env("HOME", tmp.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("no supported agent found") && err.contains("--claude"),
        "{err}"
    );
    assert!(
        std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
        "nothing was created"
    );
}
