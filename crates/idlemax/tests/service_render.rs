//! Offline server-unit rendering uses explicit target facts rather than the build host.

use std::process::Command;

#[test]
fn rendering_a_server_with_a_laptop_config_requires_target_facts() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        state.join("config.toml"),
        "[agent]\nprofile = \"macos_laptop\"\n[guard]\nram_budget_mib = 9216\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        assert!(state.is_dir() && state.starts_with(std::env::temp_dir()));
        Command::new(env!("CARGO_BIN_EXE_idlemax"))
            .args(args)
            .env("HOME", &home)
            .env("IDLEMAX_HOME", &state)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("IDLEMAX_TOKEN")
            .env_remove("IDLEMAX_COORD")
            .output()
            .unwrap()
    };
    let absent = run(&["service", "render", "agent", "--json"]);
    assert!(
        !absent.status.success(),
        "off-host render accepted local laptop facts"
    );
    assert!(String::from_utf8_lossy(&absent.stdout).contains("--cpus"));
    let explicit = run(&[
        "service",
        "render",
        "agent",
        "--cpus",
        "4",
        "--memory-mib",
        "4096",
        "--json",
    ]);
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stdout)
    );
    let unit: serde_json::Value = serde_json::from_slice(&explicit.stdout).unwrap();
    let content = unit["content"].as_str().unwrap();
    assert!(content.contains("MemoryMax=1152M"), "{content}");
    assert!(content.contains("cpus=4 memory_mib=4096"));
    assert!(content.contains("IDLEMAX_HOME=/var/lib/idlemax"));
    assert!(!content.contains("Environment=\"PATH="));
    assert!(!content.contains(home.to_str().unwrap()));
    let target = temp.path().join("target.toml");
    std::fs::write(&target, "[guard]\nmax_jobs = 1\nram_budget_mib = 512\n").unwrap();
    let pinned = run(&[
        "service",
        "render",
        "agent",
        "--cpus",
        "4",
        "--memory-mib",
        "4096",
        "--config",
        target.to_str().unwrap(),
        "--json",
    ]);
    assert!(pinned.status.success());
    let pinned: serde_json::Value = serde_json::from_slice(&pinned.stdout).unwrap();
    assert!(pinned["content"]
        .as_str()
        .unwrap()
        .contains("MemoryMax=640M"));
    let partial = run(&["service", "render", "agent", "--cpus", "4", "--json"]);
    assert!(!partial.status.success());
    assert!(!home.join("Library/LaunchAgents").exists());
}
