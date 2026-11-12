//! Tests: the child process seam: limits, groups, environment and output.
use super::*;

#[test]
fn os_commands_times_out_and_leaves_no_process() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("pgid.txt");
    let pid_file_str = pid_file.display().to_string();

    let os = OsCommands::new();
    let script = format!("echo $$ > '{pid_file_str}'; sleep 30 & sleep 30");
    let cmd = Cmd {
        program: "sh".into(),
        args: vec!["-c".into(), script],
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: Duration::from_millis(500),
        throttle: false,
        tag: None,
    };

    let start = std::time::Instant::now();
    let res = os.run(&cmd);
    let elapsed = start.elapsed();

    assert_eq!(res, Err(CmdError::TimedOut));
    assert!(
        elapsed < Duration::from_secs(2),
        "timed out too slowly: {elapsed:?}"
    );

    let content = std::fs::read_to_string(&pid_file).expect("the child wrote its pid");
    let pgid: i32 = content.trim().parse().expect("a pid");
    let check = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), None);
    assert!(check.is_err(), "processes of group {pgid} are still alive");
}

#[test]
fn build_env_filters_secrets_and_keeps_only_minimal_env() {
    let parent_vars = [
        ("IDLEMAX_TOKEN", "super-secret-token"),
        ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
        ("DATABASE_URL", "postgres://user:pass@localhost/db"),
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/test"),
        ("USER", "testuser"),
        ("LANG", "en_US.UTF-8"),
    ];

    let env_pairs = build_env(
        parent_vars,
        &[("CUSTOM_PAIR".into(), "custom_val".into())],
        Some(42),
    );

    assert!(!env_pairs.iter().any(|(k, _)| k == "IDLEMAX_TOKEN"));
    assert!(!env_pairs.iter().any(|(k, _)| k == "AWS_SECRET_ACCESS_KEY"));
    assert!(!env_pairs.iter().any(|(k, _)| k == "DATABASE_URL"));

    assert!(env_pairs
        .iter()
        .any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == "0"));
    assert!(env_pairs
        .iter()
        .any(|(k, v)| k == "IDLEMAX_JOB_TAG" && v == "42"));
    assert!(env_pairs
        .iter()
        .any(|(k, v)| k == "CUSTOM_PAIR" && v == "custom_val"));
    assert!(env_pairs
        .iter()
        .any(|(k, v)| k == "PATH" && v == "/usr/bin:/bin"));
    assert!(env_pairs
        .iter()
        .any(|(k, v)| k == "HOME" && v == "/home/test"));
}

#[test]
fn drain_capped_handles_non_utf8_lossy() {
    let bytes: &[u8] = &[0x66, 0x6f, 0x6f, 0xff, 0xfe, 0x62, 0x61, 0x72];
    let rx = drain_capped(Some(std::io::Cursor::new(bytes)));
    let out = rx.recv().unwrap();
    assert!(out.contains("foo"));
    assert!(out.contains("bar"));
}

#[test]
fn fake_commands_records_calls_and_returns_scripted_outputs() {
    let fake = FakeCommands::new();
    fake.push_output(Some(0), "first output", "");
    fake.push_error(CmdError::TimedOut);

    let cmd1 = Cmd {
        program: "git".into(),
        args: vec!["status".into()],
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: Duration::from_secs(10),
        throttle: false,
        tag: None,
    };
    let res1 = fake.run(&cmd1).unwrap();
    assert_eq!(res1.stdout, "first output");

    let res2 = fake.run(&cmd1);
    assert_eq!(res2, Err(CmdError::TimedOut));

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].program, "git");

    fake.set_fallback(|c| {
        Ok(Output {
            code: Some(0),
            stdout: format!("fallback: {}", c.program),
            stderr: String::new(),
        })
    });
    let res3 = fake.run(&cmd1).unwrap();
    assert_eq!(res3.stdout, "fallback: git");
}

#[test]
fn os_commands_with_direct_wrapper() {
    let os = OsCommands::with_options(Wrapper::Direct, 256);
    let cmd = Cmd {
        program: "true".into(),
        args: Vec::new(),
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: Duration::from_secs(5),
        throttle: true,
        tag: None,
    };
    let out = os.run(&cmd).expect("true failed");
    assert_eq!(out.code, Some(0));
}

#[test]
fn a_child_that_writes_more_than_the_cap_still_exits_cleanly() {
    let cmd = Cmd {
        program: "sh".into(),
        args: vec!["-c".into(), "head -c 2097152 /dev/zero".into()],
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: Duration::from_secs(10),
        throttle: false,
        tag: None,
    };
    let out = OsCommands::new().run(&cmd).unwrap();
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    assert!(!out.stdout.is_empty());
    assert!(out.stdout.len() <= MAX_OUTPUT_BYTES as usize);
}
