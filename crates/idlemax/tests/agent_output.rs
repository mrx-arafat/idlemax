//! What a script or agent sees from the CLI through a pipe: bare version, errors on stdout
//! with a fix, and the exit codes 0, 1 and 2. No server runs; the coordinator is a closed port.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

struct Reply {
    code: i32,
    stdout: String,
    stderr: String,
}

fn idlemax(home: &std::path::Path, args: &[&str]) -> Reply {
    let out = Command::new(BIN)
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("IDLEMAX_HOME", home.join("state"))
        .output()
        .unwrap();
    Reply {
        code: out.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn piped_output_is_bare_version_and_errors_with_a_fix_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    for flag in ["-V", "-v", "--version"] {
        let r = idlemax(home, &[flag]);
        assert_eq!(
            (r.code, r.stdout.as_str()),
            (0, concat!(env!("CARGO_PKG_VERSION"), "\n")),
            "{flag}"
        );
    }

    let r = idlemax(home, &["status"]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert!(r.stdout.starts_with("error: "), "{}", r.stdout);
    assert!(r.stdout.contains("help: idlemax init"), "{}", r.stdout);
    assert_eq!(r.stderr, "");

    let r = idlemax(home, &["init", "--coord-url", "http://127.0.0.1:1"]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    let r = idlemax(home, &["status"]);
    assert_eq!(r.code, 1);
    assert!(
        r.stdout
            .contains("coordinator unreachable at http://127.0.0.1:1"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("help: idlemax coord"), "{}", r.stdout);
    assert_eq!(r.stderr, "");

    let r = idlemax(home, &["jobs", "--stat", "queued"]);
    assert_eq!(r.code, 2);
    assert!(
        r.stdout.starts_with("error: unexpected argument '--stat'"),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("--status <STATUS>"), "{}", r.stdout);
    assert_eq!(r.stderr, "");

    let r = idlemax(home, &["--format", "table", "status"]);
    assert_eq!(
        (r.code, r.stdout.as_str()),
        (1, ""),
        "tables keep errors on stderr"
    );
    assert!(
        r.stderr.starts_with("error: coordinator unreachable"),
        "{}",
        r.stderr
    );
}
