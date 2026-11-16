//! `idlemax service` and `idlemax statusline` work before `init`, touch only the temp HOME,
//! and ask no service manager anything.

use std::path::Path;
use std::process::Command;

use idlemax_core::install::service::{unit_path, Platform, ServiceKind};
use idlemax_core::types::Os;

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
    let (ok, text) = idlemax(
        &home,
        &["--format", "table", "service", "install", "--dry-run"],
    );
    assert!(ok && text.contains("dry run"), "{text}");
    assert!(!home.join("Library").exists() && !home.join(".config").exists());

    // Both services installed: a bare `service install` lists the coordinator first and writes nothing.
    let platform = Platform::for_os(Os::current()).unwrap();
    let (coord, agent) = (
        unit_path(platform, ServiceKind::Coord, &home),
        unit_path(platform, ServiceKind::Agent, &home),
    );
    std::fs::create_dir_all(coord.parent().unwrap()).unwrap();
    std::fs::write(&coord, "stub\n").unwrap();
    std::fs::write(&agent, "stub\n").unwrap();
    let (ok, text) = idlemax(
        &home,
        &["--format", "table", "service", "install", "--dry-run"],
    );
    let at = |p: &Path| text.find(&format!("file: {}", p.display()));
    assert!(
        ok && at(&coord).is_some() && at(&agent).is_some() && at(&coord) < at(&agent),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&coord).unwrap(), "stub\n");
    assert_eq!(std::fs::read_to_string(&agent).unwrap(), "stub\n");
}
