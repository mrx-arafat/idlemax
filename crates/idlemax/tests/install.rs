//! `idlemax service` and `idlemax statusline` work before `init`, touch only the temp HOME,
//! and ask no service manager anything.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const SETTINGS: &str = "{\n  \"model\": \"opus\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"~/.claude/statusline.sh\"\n  }\n}\n";

fn idlemax(home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(BIN)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("IDLEMAX_HOME", home.join("state"))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&[out.stdout, out.stderr].concat()).into_owned();
    (out.status.success(), text)
}

#[test]
fn statusline_round_trips_and_service_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, SETTINGS).unwrap();

    let (ok, text) = idlemax(&home, &["statusline", "install"]);
    assert!(ok, "{text}");
    assert_ne!(std::fs::read_to_string(&settings).unwrap(), SETTINGS);
    let (ok, text) = idlemax(&home, &["statusline", "uninstall"]);
    assert!(ok, "{text}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), SETTINGS);

    let (ok, text) = idlemax(&home, &["--json", "service", "status"]);
    assert!(ok && text.contains("\"installed\": false"), "{text}");
    let (ok, text) = idlemax(&home, &["service", "install", "--dry-run"]);
    assert!(ok && text.contains("dry run"), "{text}");
    assert!(!home.join("Library").exists() && !home.join(".config").exists());
}
