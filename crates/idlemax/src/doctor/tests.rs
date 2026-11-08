//! Doctor adapter tests: temp state dirs and fixture files, never real user state.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use idlemax_core::doctor::{Check, Level, Report};
use idlemax_core::protocol::LocalStatus;
use idlemax_core::quota::{AccountConfig, Provider};
use idlemax_core::types::{ProbeSource, QuotaSnapshot, QuotaWindowKind};
use idlemax_core::{Config, Paths};

use super::{ai, checks, host, net, service};

fn state_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn paths_in(tmp: &tempfile::TempDir) -> Paths {
    let paths = Paths::new(tmp.path().join("state"));
    std::fs::create_dir_all(paths.root()).unwrap();
    paths
}

fn one(report: &Report, id: &str) -> Check {
    let found: Vec<&Check> = report.checks.iter().filter(|c| c.id == id).collect();
    assert_eq!(found.len(), 1, "{id} in {:?}", report.checks);
    found[0].clone()
}

fn write_token(paths: &Paths, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(paths.token_file(), "a".repeat(64)).unwrap();
    std::fs::set_permissions(paths.token_file(), std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn token_missing_public_and_private() {
    let tmp = state_dir();
    let paths = paths_in(&tmp);
    let mut report = Report::new("t", None);
    assert_eq!(checks::token_file(&mut report, &paths.token_file()), None);
    let check = one(&report, "token");
    assert_eq!(check.level, Level::Fail);
    assert!(check.detail.contains("no token file"));
    assert!(check.fix.unwrap().contains("idlemax init"));

    write_token(&paths, 0o644);
    let mut report = Report::new("t", None);
    assert_eq!(checks::token_file(&mut report, &paths.token_file()), None);
    let check = one(&report, "token");
    assert_eq!(check.level, Level::Fail);
    assert!(check.detail.contains("mode 0644"));
    assert!(check
        .fix
        .unwrap()
        .contains(paths.token_file().to_string_lossy().as_ref()));

    write_token(&paths, 0o600);
    let mut report = Report::new("t", None);
    let token = checks::token_file(&mut report, &paths.token_file());
    assert_eq!(token.as_deref(), Some("a".repeat(64).as_str()));
    assert_eq!(one(&report, "token").level, Level::Ok);
}

#[test]
fn token_file_too_short_is_a_fail_that_says_how_long_it_must_be() {
    let tmp = state_dir();
    let paths = paths_in(&tmp);
    write_token(&paths, 0o600);
    std::fs::write(paths.token_file(), "a\n").unwrap();
    let mut report = Report::new("t", None);
    assert_eq!(checks::token_file(&mut report, &paths.token_file()), None);
    let token = one(&report, "token");
    assert_eq!(token.level, Level::Fail);
    assert!(
        token.detail.contains("1 characters") || token.detail.contains("at least 32"),
        "{}",
        token.detail
    );
    assert!(token.fix.unwrap().contains("idlemax init"));
}

#[test]
fn token_file_empty_is_a_fail() {
    let tmp = state_dir();
    let paths = paths_in(&tmp);
    write_token(&paths, 0o600);
    std::fs::write(paths.token_file(), "  \n").unwrap();
    let mut report = Report::new("t", None);
    assert_eq!(checks::token_file(&mut report, &paths.token_file()), None);
    let check = one(&report, "token");
    assert_eq!(check.level, Level::Fail);
    assert!(check.detail.contains("empty"));
    assert!(check.fix.unwrap().contains("--token-stdin"));
}

#[test]
fn config_reports_missing_unknown_and_broken() {
    let tmp = state_dir();
    let paths = paths_in(&tmp);

    let mut report = Report::new("t", None);
    checks::config(&mut report, Some(&paths));
    let check = one(&report, "config");
    assert_eq!(check.level, Level::Warn);
    assert!(check.detail.contains("no "));
    assert!(check.fix.unwrap().contains("idlemax init"));

    std::fs::write(paths.config_file(), "[agent]\nsample_sec = 5\n").unwrap();
    let mut report = Report::new("t", None);
    checks::config(&mut report, Some(&paths));
    let check = one(&report, "config");
    assert_eq!(check.level, Level::Warn);
    assert!(check.detail.contains("agent.sample_sec"));
    assert!(check.fix.unwrap().contains("unknown keys"));

    std::fs::write(paths.config_file(), "[agent\n").unwrap();
    let mut report = Report::new("t", None);
    checks::config(&mut report, Some(&paths));
    let check = one(&report, "config");
    assert_eq!(check.level, Level::Fail);
    assert!(check.detail.contains("line 1"));
    assert!(check.fix.unwrap().contains("fix the key or value"));

    std::fs::write(
        paths.config_file(),
        Config::default().to_toml_string().unwrap(),
    )
    .unwrap();
    let mut report = Report::new("t", None);
    checks::config(&mut report, Some(&paths));
    assert_eq!(one(&report, "config").level, Level::Ok);
}

fn executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn allowlist_finds_each_broken_entry() {
    let tmp = state_dir();
    let good = tmp.path().join("good");
    executable(&good);
    let flat = tmp.path().join("flat");
    std::fs::write(&flat, "x").unwrap();
    let mut cfg = Config::default();
    cfg.agent.command_allowlist = vec![good.to_string_lossy().into_owned(), "sh".into()];
    let mut report = Report::new("t", None);
    host::allowlist(&mut report, &cfg, None);
    assert_eq!(one(&report, "allowlist").level, Level::Ok);

    cfg.agent.command_allowlist = vec![
        tmp.path().join("gone").to_string_lossy().into_owned(),
        flat.to_string_lossy().into_owned(),
        "./relative".into(),
        "idlemax-no-such-binary".into(),
    ];
    let mut report = Report::new("t", None);
    host::allowlist(&mut report, &cfg, None);
    let check = one(&report, "allowlist");
    assert_eq!(check.level, Level::Fail);
    assert_eq!(check.detail.matches("will not run").count(), 1);
    for needle in [
        "(missing)",
        "(not executable)",
        "(relative path)",
        "(not on PATH)",
    ] {
        assert!(
            check.detail.contains(needle),
            "{needle} in {}",
            check.detail
        );
    }
    let fix = check.fix.unwrap();
    assert!(fix.contains("install "));
    assert!(fix.contains("chmod +x "));
    assert!(fix.contains("absolute path"));
    assert!(fix.contains("agent.command_allowlist"));

    cfg.agent.command_allowlist = Vec::new();
    let mut report = Report::new("t", None);
    host::allowlist(&mut report, &cfg, None);
    let check = one(&report, "allowlist");
    assert_eq!(check.level, Level::Warn);
    assert!(check.detail.contains("no job can start"));
}

#[test]
fn service_absent_placeholder_and_installed() {
    let home = tempfile::tempdir().unwrap();
    let mut report = Report::new("t", None);
    service::check(&mut report, &service::installed(Some(home.path())));
    let check = one(&report, "service");
    assert_eq!(check.level, Level::Warn, "{}", check.detail);
    assert!(check.fix.unwrap().contains("idlemax service install"));

    let unit = match cfg!(target_os = "linux") {
        true => home
            .path()
            .join(".config/systemd/user/idlemax-agent.service"),
        _ => home
            .path()
            .join("Library/LaunchAgents/com.idlemax.agent.plist"),
    };
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, "<string>@BIN@</string>").unwrap();
    let mut report = Report::new("t", None);
    service::check(&mut report, &service::installed(Some(home.path())));
    let check = one(&report, "service");
    assert_eq!(check.level, Level::Warn);
    assert!(check.detail.contains("@BIN@"));
    assert!(check.fix.unwrap().contains("idlemax service install"));

    std::fs::write(&unit, "<string>/usr/local/bin/idlemax</string>").unwrap();
    let mut report = Report::new("t", None);
    service::check(&mut report, &service::installed(Some(home.path())));
    assert_eq!(one(&report, "service").level, Level::Ok);
}

#[test]
fn a_unit_whose_binary_is_gone_fails_and_one_that_runs_passes() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("gone/idlemax");
    let real = home.path().join("bin/idlemax");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "#!/bin/sh\n").unwrap();
    executable(&real);
    let unit = match cfg!(target_os = "linux") {
        true => home
            .path()
            .join(".config/systemd/user/idlemax-agent.service"),
        _ => home
            .path()
            .join("Library/LaunchAgents/com.idlemax.agent.plist"),
    };
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    let write = |bin: &Path| {
        let text = match cfg!(target_os = "linux") {
            true => format!("[Service]\nExecStart={} agent\n", bin.display()),
            _ => format!(
                "<key>ProgramArguments</key>\n<array>\n<string>{}</string>\n<string>agent</string>\n</array>\n",
                bin.display()
            ),
        };
        std::fs::write(&unit, text).unwrap();
        let mut report = Report::new("t", None);
        service::check(&mut report, &service::installed(Some(home.path())));
        one(&report, "service")
    };
    let broken = write(&missing);
    assert_eq!(broken.level, Level::Fail, "{}", broken.detail);
    assert!(broken.detail.contains("gone/idlemax"), "{}", broken.detail);
    assert!(broken.fix.unwrap().contains("service install --bin"));
    assert_eq!(write(&real).level, Level::Ok);
}

#[test]
fn an_agent_that_does_not_answer_is_a_failure_only_when_its_service_is_installed() {
    let url = "http://127.0.0.1:7879";
    let hint = net::agent_down(url, false);
    assert_eq!(hint.level, Level::Warn);
    let down = net::agent_down(url, true);
    assert_eq!(down.level, Level::Fail);
    assert!(down.fix.unwrap().contains("idlemax service status"));
}

fn account(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.into(),
        provider: Provider::Claude,
        probe: Some(ProbeSource::Manual),
        probe_path: Some("/nowhere".into()),
        ..AccountConfig::default()
    }
}

fn snapshot(account: &str, at_ms: i64) -> QuotaSnapshot {
    QuotaSnapshot {
        account: account.into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 10.0,
        resets_at_ms: None,
        source: ProbeSource::Manual,
        at_ms,
    }
}

#[test]
fn ai_account_stale_fresh_and_device_note() {
    let account = account("work");
    let now = 1_800_000_000_000;
    let mut cfg = Config::default();

    let fresh = ai::account_check(
        &cfg,
        &account,
        Ok(vec![snapshot("work", now - 60_000)]),
        &[],
        now,
    );
    assert_eq!(fresh.level, Level::Ok);
    assert!(fresh.detail.contains("1m old"), "{}", fresh.detail);
    assert!(fresh.detail.contains("not in agent.ai_accounts"));

    let stale = ai::account_check(
        &cfg,
        &account,
        Ok(vec![snapshot("work", now - 7 * 60 * 60 * 1000)]),
        &[],
        now,
    );
    assert_eq!(stale.level, Level::Warn);
    assert!(stale.detail.contains("7h00m old"), "{}", stale.detail);
    assert!(stale.fix.unwrap().contains("quota-work.json"));

    cfg.agent.ai_accounts = vec!["work".into()];
    let fresh = ai::account_check(
        &cfg,
        &account,
        Ok(vec![snapshot("work", now - 60_000)]),
        &[],
        now,
    );
    assert!(!fresh.detail.contains("agent.ai_accounts"));
}

#[test]
fn ai_account_missing_probe_is_a_warning_with_the_path() {
    let account = account("work");
    let now = 1_800_000_000_000;
    let cfg = Config::default();
    let err = crate::agent::quota_probe::probe_account(
        &account,
        &crate::agent::quota_probe::ProbeDirs {
            state_dir: PathBuf::from("/nowhere"),
            ..Default::default()
        },
    )
    .err()
    .unwrap();
    let check = ai::account_check(&cfg, &account, Err(err), &[], now);
    assert_eq!(check.level, Level::Warn);
    assert!(
        check.detail.contains("no manual quota file"),
        "{}",
        check.detail
    );
    assert!(check.detail.contains("/nowhere"), "{}", check.detail);
    assert!(check.fix.unwrap().contains("quota-work.json"));
}

#[test]
fn a_claude_account_without_a_capture_points_at_statusline_install() {
    let mut account = account("claude-max");
    account.probe = Some(ProbeSource::ClaudeStatusline);
    account.probe_path = None;
    let state = tempfile::tempdir().unwrap();
    let dirs = crate::agent::quota_probe::ProbeDirs {
        state_dir: state.path().to_path_buf(),
        ..Default::default()
    };
    let err = crate::agent::quota_probe::probe_account(&account, &dirs)
        .err()
        .unwrap();
    let check = ai::account_check(
        &Config::default(),
        &account,
        Err(err),
        &[],
        1_800_000_000_000,
    );
    assert_eq!(check.level, Level::Warn, "{}", check.detail);
    assert!(check.fix.unwrap().contains("idlemax statusline install"));
}

#[test]
fn agent_status_is_ok_only_with_this_version_and_a_reachable_coordinator() {
    let status = |version: &str, coordinator_reachable| LocalStatus {
        device: "mac".into(),
        version: version.into(),
        coordinator_reachable,
        ..LocalStatus::default()
    };
    let url = "http://127.0.0.1:7879";

    let ok = net::agent_status_check(url, &status(idlemax_core::VERSION, true));
    assert_eq!(ok.level, Level::Ok);
    assert!(ok.detail.ends_with("0 running, coordinator reachable"));

    let cut_off = net::agent_status_check(url, &status(idlemax_core::VERSION, false));
    assert_eq!(cut_off.level, Level::Warn);
    assert!(cut_off.detail.contains("cannot reach the coordinator"));
    assert!(cut_off.fix.unwrap().contains("coordinator.url"));

    let old = net::agent_status_check(url, &status("0.0.1", true));
    assert_eq!(old.level, Level::Warn);
    assert!(old.fix.unwrap().contains("restart the agent"));
}

#[test]
fn pause_check_names_each_hold_with_its_own_fix() {
    let status = |kill_switch, local_paused| LocalStatus {
        kill_switch,
        local_paused,
        ..LocalStatus::default()
    };
    let file = Path::new("/state/PAUSE");

    let free = net::pause_check(&status(false, false), file);
    assert_eq!((free.id.as_str(), free.level), ("pause", Level::Ok));

    let switch = net::pause_check(&status(true, false), file);
    assert_eq!(switch.level, Level::Warn);
    assert!(switch.detail.contains("/state/PAUSE"), "{}", switch.detail);
    assert_eq!(switch.fix.as_deref(), Some("rm /state/PAUSE"));

    let local = net::pause_check(&status(false, true), file);
    assert_eq!(local.level, Level::Warn);
    assert_eq!(local.fix.as_deref(), Some("idlemax local resume"));

    let both = net::pause_check(&status(true, true), file);
    let fix = both.fix.unwrap();
    assert!(fix.contains("rm /state/PAUSE") && fix.contains("idlemax local resume"));
}

#[test]
fn home_check_warns_when_other_users_can_reach_the_state_dir() {
    let ok = checks::home_check("/s/.idlemax", "default", Some(0o700));
    assert_eq!(ok.level, Level::Ok);
    assert_eq!(
        checks::home_check("/s/.idlemax", "default", None).level,
        Level::Ok
    );

    for open in [0o755, 0o750, 0o705] {
        let warn = checks::home_check("/s/.idlemax", "IDLEMAX_HOME", Some(open));
        assert_eq!(warn.level, Level::Warn, "{open:o}");
        assert!(
            warn.detail.contains(&format!("{open:04o}")),
            "{}",
            warn.detail
        );
        assert_eq!(warn.fix.as_deref(), Some("chmod 700 /s/.idlemax"));
    }
}

#[test]
fn a_misdirected_coordinator_reply_tells_how_to_allow_the_name() {
    use crate::cli::ClientError;
    let refused = ClientError::Api {
        status: 421,
        path: "/v1/status".into(),
        message: "host \"server:7878\" is not allowed".into(),
    };
    let check = net::coordinator_error("http://server:7878", &refused);
    assert_eq!(check.level, Level::Fail);
    let fix = check.fix.unwrap();
    assert!(
        fix.contains("allowed_hosts") && fix.contains("IP address"),
        "{fix}"
    );
}

#[test]
fn allowlist_resolves_bare_names_on_the_path_the_service_was_installed_with() {
    let mut cfg = Config::default();
    cfg.agent.command_allowlist = vec!["sh".into()];
    let check = |path: Option<&str>| {
        let mut report = Report::new("t", None);
        host::allowlist(&mut report, &cfg, path);
        one(&report, "allowlist")
    };
    // `sh` is on this process's PATH, but not on a PATH the unit froze without it.
    assert_eq!(check(None).level, Level::Ok);
    let broken = check(Some("/nonexistent-dir"));
    assert_eq!(broken.level, Level::Fail);
    assert!(
        broken.detail.contains("not on the service's PATH"),
        "{}",
        broken.detail
    );
    assert!(broken.fix.unwrap().contains("idlemax service install"));
    let ok = check(Some("/bin:/usr/bin"));
    assert_eq!(ok.level, Level::Ok);
    assert!(ok.detail.contains("on the service's PATH"), "{}", ok.detail);
}
