//! `idlemax config` through the real binary: it edits config.toml, keeps a backup and the
//! user's comments, refuses a bad key or value with exit 2, and says when no agent is running
//! to reload. No server runs; the agent address is a closed loopback port.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

struct Reply {
    code: i32,
    stdout: String,
}

fn idlemax(home: &std::path::Path, args: &[&str]) -> Reply {
    let out = Command::new(BIN)
        .args(args)
        .env_clear()
        // `env_clear` already drops it; named so the lock's marker is never inherited.
        .env_remove("IDLEMAX_JOB_ID")
        .env("HOME", home)
        .env("IDLEMAX_HOME", home.join("state"))
        .output()
        .unwrap();
    Reply {
        code: out.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

#[test]
fn config_set_edits_the_file_keeps_comments_and_a_backup_and_refuses_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    assert_eq!(idlemax(home, &["init"]).code, 0);
    let file = home.join("state/config.toml");
    // A laptop profile, so the defaults are the same on every OS; the agent address is a port
    // nothing listens on; and a comment the edits must keep.
    std::fs::write(
        &file,
        "# my notes\n[agent]\nprofile = \"laptop\"\nlisten = \"127.0.0.1:1\"\n",
    )
    .unwrap();

    let set = idlemax(home, &["config", "set", "guard.require_ac_power", "false"]);
    assert_eq!(set.code, 0, "{}", set.stdout);
    assert!(
        set.stdout.contains("guard.require_ac_power: true -> false"),
        "{}",
        set.stdout
    );
    assert!(set.stdout.contains("not running"), "{}", set.stdout);
    let edited = std::fs::read_to_string(&file).unwrap();
    assert!(
        edited.contains("require_ac_power = false") && edited.contains("# my notes"),
        "{edited}"
    );
    assert!(home.join("state/config.toml.idlemax.bak").is_file());

    // Setting it again is a no-op, and the file is what `list` and `get` report.
    let again = idlemax(home, &["config", "set", "guard.require_ac_power", "false"]);
    assert_eq!(
        (again.code, again.stdout.contains("(no-op)")),
        (0, true),
        "{}",
        again.stdout
    );
    let list = idlemax(home, &["config", "list", "--json"]);
    assert!(
        list.stdout.contains("\"guard.require_ac_power\""),
        "{}",
        list.stdout
    );
    assert!(idlemax(home, &["config", "get", "guard.require_ac_power"])
        .stdout
        .contains("value: false"));

    let list_text = idlemax(
        home,
        &[
            "config",
            "set",
            "agent.command_allowlist",
            "git,/usr/bin/make",
        ],
    );
    assert_eq!(list_text.code, 0, "{}", list_text.stdout);
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("command_allowlist = [\"git\", \"/usr/bin/make\"]"));

    // Unset restores the default and removes the line.
    assert_eq!(
        idlemax(home, &["config", "unset", "guard.require_ac_power"]).code,
        0
    );
    assert!(!std::fs::read_to_string(&file)
        .unwrap()
        .contains("require_ac_power"));

    // A typo, a wrong value and a list of tables are usage errors with the fix on stdout.
    for (args, expect) in [
        (
            ["config", "set", "agent.sample_sec", "5"],
            "did you mean `agent.sample_secs`?",
        ),
        (
            ["config", "set", "agent.notify", "banana"],
            "cannot set `agent.notify` to `banana`",
        ),
        (["config", "set", "quota.accounts", "x"], "list of tables"),
    ] {
        let reply = idlemax(home, &args);
        assert_eq!(reply.code, 2, "{args:?}: {}", reply.stdout);
        assert!(
            reply.stdout.contains(expect) && reply.stdout.contains("help:"),
            "{}",
            reply.stdout
        );
    }
}
