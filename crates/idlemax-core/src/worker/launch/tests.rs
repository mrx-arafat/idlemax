//! Tests: the launch arguments, the pane's pairs, the environment check and the host choice.

use super::*;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn claude_starts_lean_with_its_session_id_and_resumes_it() {
    let first = agent_args(WorkerCli::Claude, "sonnet", Session::New("u-1"), "/g", &[]);
    assert_eq!(
        first.unwrap(),
        strings(&[
            "--model",
            "sonnet",
            "--strict-mcp-config",
            "--permission-mode",
            "bypassPermissions",
            "--session-id",
            "u-1"
        ])
    );
    let again = agent_args(
        WorkerCli::Claude,
        "sonnet",
        Session::Resume("u-1"),
        "/g",
        &[],
    );
    assert_eq!(again.unwrap()[5..], strings(&["--resume", "u-1"]));
}

#[test]
fn codex_runs_in_its_sandbox_with_each_mcp_server_switched_off() {
    let servers = strings(&["github", "notes"]);
    let args = agent_args(
        WorkerCli::Codex,
        "gpt",
        Session::New("x"),
        "/r/.git",
        &servers,
    );
    let want = strings(&[
        "--model",
        "gpt",
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
        "never",
        "--add-dir",
        "/r/.git",
        "-c",
        "mcp_servers.github.enabled=false",
        "-c",
        "mcp_servers.notes.enabled=false",
    ]);
    assert_eq!(args.unwrap(), want);
    let resumed = agent_args(
        WorkerCli::Codex,
        "gpt",
        Session::Resume("s9"),
        "/r/.git",
        &[],
    );
    assert_eq!(resumed.unwrap()[..2], strings(&["resume", "s9"]));
}

#[test]
fn opencode_and_agy_are_refused_naming_what_is_missing() {
    for cli in [WorkerCli::Opencode, WorkerCli::Agy] {
        let err = agent_args(cli, "m", Session::New("x"), "/g", &[]).unwrap_err();
        assert!(err.to_string().contains("MCP servers"), "{err}");
        assert!(err.to_string().starts_with(cli.as_str()), "{err}");
    }
}

#[test]
fn a_reviewer_runs_quiet_and_read_only() {
    let claude = reviewer_args(WorkerCli::Claude, "opus", "/s.json", "/o.json").unwrap();
    assert_eq!(claude[..3], strings(&["claude", "-p", "--settings"]));
    assert!(claude.contains(&"--strict-mcp-config".to_string()));
    assert!(claude.contains(&REVIEW_SCHEMA.to_string()));
    assert_eq!(claude[claude.len() - 2..], strings(&["--model", "opus"]));
    let codex = reviewer_args(WorkerCli::Codex, "gpt", "/s.json", "/o.json").unwrap();
    assert_eq!(
        codex,
        strings(&[
            "codex",
            "exec",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--ignore-user-config",
            "--output-schema",
            "/s.json",
            "-o",
            "/o.json",
            "-m",
            "gpt"
        ])
    );
    assert!(reviewer_args(WorkerCli::Agy, "m", "/s", "/o").is_err());
}

#[test]
fn codex_mcp_list_gives_the_first_column_or_fails_closed() {
    let list = "Name    Command  Args\ngithub  npx      -y gh\n\nnotes   node     n.js\n";
    assert_eq!(
        mcp_server_names(list).unwrap(),
        strings(&["github", "notes"])
    );
    assert!(mcp_server_names(&format!("{NO_MCP_SERVERS}\n"))
        .unwrap()
        .is_empty());
    let named_name = "Name  Command\nname  npx\n";
    assert_eq!(mcp_server_names(named_name).unwrap(), strings(&["name"]));
    for bad in [
        "Name  Command\nmy.server  npx\n",
        "No MCP servers here, maybe\n",
        "github  npx\n",
        "",
    ] {
        assert!(mcp_server_names(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn env_problems_names_credentials_and_nothing_else() {
    let vars = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<Vec<_>>()
    };
    let bad = vars(&[
        ("IDLEMAX_TOKEN", "t"),
        ("GH_TOKEN", "g"),
        ("SSH_AUTH_SOCK", "/s"),
        ("ANTHROPIC_API_KEY", "k"),
        ("DATABASE_URL", "pg://"),
    ]);
    assert_eq!(
        env_problems(&bad),
        strings(&[
            "IDLEMAX_TOKEN",
            "GH_TOKEN",
            "SSH_AUTH_SOCK",
            "ANTHROPIC_API_KEY",
            "DATABASE_URL"
        ])
    );
    let fine = vars(&[
        ("PATH", "/bin"),
        ("HOME", "/h"),
        ("TERM", "xterm"),
        ("HERDR_PANE_ID", "p1"),
        ("IDLEMAX_JOB_ID", "7"),
        ("GH_TOKEN", ""),
    ]);
    assert!(env_problems(&fine).is_empty());
}

#[test]
fn pane_env_blanks_the_given_names_and_carries_the_job_id() {
    let env = pane_env(7, 99, &strings(&["origin"]), &strings(&["FOO_TOKEN"]));
    let has = |k: &str, v: &str| env.iter().any(|(a, b)| a == k && b == v);
    assert!(has("FOO_TOKEN", ""));
    assert!(has("IDLEMAX_JOB_ID", "7"));
    assert!(has("IDLEMAX_JOB_TAG", "99"));
    assert!(has("IDLEMAX_TOKEN", ""));
    assert!(has("GIT_CONFIG_COUNT", "11"));
    assert!(has("GIT_ASKPASS", "") && has("SSH_ASKPASS", ""));
    assert!(has("GIT_CONFIG_KEY_1", "remote.origin.pushurl"));
    assert!(has("GIT_CONFIG_VALUE_1", "idlemax-no-push://origin"));
    assert!(env_problems(&env).is_empty());
}

#[test]
fn headless_args_matches_claude_start_and_resume_and_refuses_codex() {
    let first = headless_args(WorkerCli::Claude, "sonnet", Session::New("u-1")).unwrap();
    assert_eq!(
        first,
        strings(&[
            "-p",
            "--model",
            "sonnet",
            "--strict-mcp-config",
            "--permission-mode",
            "bypassPermissions",
            "--session-id",
            "u-1"
        ])
    );
    let resumed = headless_args(WorkerCli::Claude, "sonnet", Session::Resume("u-1")).unwrap();
    assert_eq!(
        resumed,
        strings(&[
            "-p",
            "--model",
            "sonnet",
            "--strict-mcp-config",
            "--permission-mode",
            "bypassPermissions",
            "--resume",
            "u-1"
        ])
    );
    let codex = headless_args(WorkerCli::Codex, "gpt", Session::New("s-1")).unwrap_err();
    assert!(codex.to_string().starts_with("codex"), "{codex}");
    assert!(codex.to_string().contains("session id"), "{codex}");
    assert!(headless_args(WorkerCli::Agy, "m", Session::New("s-1")).is_err());
}

#[test]
fn pick_host_rules_for_production_auto_and_explicit_choices() {
    for choice in [
        WorkerHostChoice::Auto,
        WorkerHostChoice::Herdr,
        WorkerHostChoice::Headless,
    ] {
        assert_eq!(
            pick_host(choice, Some(DeviceProfile::Production), true),
            WorkerHostChoice::Headless
        );
        assert_eq!(
            pick_host(choice, Some(DeviceProfile::Production), false),
            WorkerHostChoice::Headless
        );
    }
    assert_eq!(
        pick_host(WorkerHostChoice::Auto, None, false),
        WorkerHostChoice::Headless
    );
    assert_eq!(
        pick_host(WorkerHostChoice::Auto, None, true),
        WorkerHostChoice::Herdr
    );
    assert_eq!(
        pick_host(WorkerHostChoice::Headless, None, true),
        WorkerHostChoice::Headless
    );
    assert_eq!(
        pick_host(WorkerHostChoice::Herdr, None, false),
        WorkerHostChoice::Herdr
    );
}
