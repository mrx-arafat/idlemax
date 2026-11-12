//! Adapter: checks whether required agent CLIs are logged in.

use std::time::Duration;

use idlemax_core::doctor::Check;
use idlemax_core::quota::login::{claude_login, codex_login, Login};
use idlemax_core::Config;

use super::facts::{self, CmdOutcome};

/// Timeout for CLI login status commands in doctor.
pub const DOCTOR_LOGIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs login checks for each unique agent CLI required by configured quota accounts.
pub async fn check(report: &mut idlemax_core::doctor::Report, cfg: &Config) {
    if cfg.quota.accounts.is_empty() {
        return;
    }
    let mut clis: Vec<&'static str> = Vec::new();
    for account in &cfg.quota.accounts {
        if let Some(cli) = account.provider.cli() {
            if !clis.contains(&cli) {
                clis.push(cli);
            }
        }
    }
    for cli in clis {
        report.push(check_cli(cli).await);
    }
}

async fn check_cli(cli: &'static str) -> Check {
    let id = format!("login.{cli}");
    match cli {
        "claude" => {
            check_cli_with_cmd(cli, &["auth", "status", "--json"], "claude auth login").await
        }
        "codex" => check_cli_with_cmd(cli, &["login", "status"], "codex login").await,
        _ => Check::ok(id, "idlemax cannot check its login"),
    }
}

async fn check_cli_with_cmd(cli: &str, status_args: &[&str], login_fix_cmd: &str) -> Check {
    let mut cmd = tokio::process::Command::new(cli);
    cmd.args(status_args);
    let outcome = facts::output(cmd, DOCTOR_LOGIN_TIMEOUT).await;
    evaluate_outcome(cli, outcome, status_args, login_fix_cmd)
}

pub(crate) fn evaluate_outcome(
    cli: &str,
    outcome: CmdOutcome,
    status_args: &[&str],
    login_fix_cmd: &str,
) -> Check {
    let id = format!("login.{cli}");
    let status_cmd = format!("{cli} {}", status_args.join(" "));
    match outcome {
        CmdOutcome::Missing => Check::warn(
            id,
            format!("needs you: {cli} is not installed"),
            format!("install {cli} and log in"),
        ),
        CmdOutcome::Timeout => Check::warn(
            id,
            format!("cannot tell whether {cli} is logged in: timed out"),
            format!("run `{status_cmd}` by hand to check its login"),
        ),
        CmdOutcome::Spawn(e) => Check::warn(
            id,
            format!("cannot tell whether {cli} is logged in: {e}"),
            format!("run `{status_cmd}` by hand to check its login"),
        ),
        CmdOutcome::Ran { code, stdout, .. } => {
            let login = match cli {
                "claude" => claude_login(&stdout),
                _ => codex_login(code, &stdout),
            };
            match login {
                Login::In { id: org_id } => {
                    let detail = match org_id {
                        Some(id) => {
                            let prefix: String = id.chars().take(8).collect();
                            format!("logged in ({prefix})")
                        }
                        None => "logged in".to_string(),
                    };
                    Check::ok(id, detail)
                }
                Login::Out => Check::warn(
                    id,
                    format!("needs you: no worker can use {cli} on this device until logged in"),
                    format!("{login_fix_cmd}, run as the user the agent runs as"),
                ),
                Login::Unknown { reason } => Check::warn(
                    id,
                    format!("cannot tell whether {cli} is logged in: {reason}"),
                    format!("run `{status_cmd}` by hand to check its login"),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::doctor::{Level, Report};
    use idlemax_core::quota::{AccountConfig, Provider};

    use super::*;

    #[tokio::test]
    async fn test_check_empty_accounts_adds_no_checks() {
        let mut report = Report::new("0.2.0", None);
        let cfg = Config::default();
        check(&mut report, &cfg).await;
        assert!(!report.checks.iter().any(|c| c.id.starts_with("login.")));
    }

    #[tokio::test]
    async fn test_check_agy_has_no_status_cmd() {
        let mut report = Report::new("0.2.0", None);
        let mut cfg = Config::default();
        cfg.quota.accounts.push(AccountConfig {
            name: "agy-account".to_string(),
            provider: Provider::Agy,
            ..Default::default()
        });
        check(&mut report, &cfg).await;
        let c = report
            .checks
            .iter()
            .find(|c| c.id == "login.agy")
            .expect("must have login.agy check");
        assert_eq!(c.level, Level::Ok);
        assert_eq!(c.detail, "idlemax cannot check its login");
    }

    #[test]
    fn test_evaluate_outcome_missing() {
        let c = evaluate_outcome(
            "claude",
            CmdOutcome::Missing,
            &["auth", "status", "--json"],
            "claude auth login",
        );
        assert_eq!(c.id, "login.claude");
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.starts_with("needs you:"));
        assert!(c.detail.contains("claude"));
    }

    #[test]
    fn test_evaluate_outcome_timeout() {
        let c = evaluate_outcome(
            "claude",
            CmdOutcome::Timeout,
            &["auth", "status", "--json"],
            "claude auth login",
        );
        assert_eq!(c.id, "login.claude");
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.starts_with("cannot tell whether"));
    }

    #[test]
    fn test_evaluate_outcome_in_and_out() {
        let in_check = evaluate_outcome(
            "claude",
            CmdOutcome::Ran {
                code: Some(0),
                stdout: r#"{"loggedIn": true, "orgId": "org-123456789"}"#.to_string(),
                stderr: String::new(),
            },
            &["auth", "status", "--json"],
            "claude auth login",
        );
        assert_eq!(in_check.level, Level::Ok);
        assert_eq!(in_check.detail, "logged in (org-1234)");

        let out_check = evaluate_outcome(
            "claude",
            CmdOutcome::Ran {
                code: Some(1),
                stdout: r#"{"loggedIn": false}"#.to_string(),
                stderr: String::new(),
            },
            &["auth", "status", "--json"],
            "claude auth login",
        );
        assert_eq!(out_check.level, Level::Warn);
        assert!(out_check.detail.starts_with("needs you:"));
        assert_eq!(
            out_check.fix.as_deref(),
            Some("claude auth login, run as the user the agent runs as")
        );
    }
}
