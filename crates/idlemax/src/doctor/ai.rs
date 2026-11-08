//! Adapter: one check per configured AI account, read from its quota probe.

use idlemax_core::doctor::{probe_verdict, Check, ProbeVerdict, Report};
use idlemax_core::quota::{self, AccountConfig, SpendContext, SNAPSHOT_MAX_AGE_MS};
use idlemax_core::types::{ProbeSource, QuotaSnapshot, UnixMs};
use idlemax_core::{Config, Paths};

use super::checks::AFTER_HOME;
use crate::agent::quota_probe::{self, ProbeDirs, ProbeError};
use crate::cli::rows;
use crate::state::now_ms;

/// Check 7: every configured account's probe is there and fresh, or names why it is not.
pub async fn accounts(report: &mut Report, cfg: &Config, paths: Option<&Paths>) {
    if cfg.quota.accounts.is_empty() {
        report.push(Check::ok(
            "ai",
            "no AI accounts configured: only the rules advisor runs",
        ));
        return;
    }
    let Some(paths) = paths else {
        report.push(Check::warn("ai", "not checked: no state dir", AFTER_HOME));
        return;
    };
    let now = now_ms();
    let dirs = ProbeDirs::from_env(paths);
    let (quota_cfg, probe_dirs) = (cfg.quota.clone(), dirs.clone());
    let probed = tokio::task::spawn_blocking(move || {
        let interactive = quota_probe::interactive_use(&probe_dirs, now);
        let results = quota_cfg
            .accounts
            .iter()
            .map(|account| quota_probe::probe_account(account, &probe_dirs))
            .collect::<Vec<_>>();
        (results, interactive)
    })
    .await;
    let (results, interactive) = match probed {
        Ok(probed) => probed,
        Err(e) => {
            report.push(Check::fail(
                "ai",
                format!("the quota probe task failed: {e}"),
                "run `idlemax doctor` again; report it if it repeats",
            ));
            return;
        }
    };
    for (account, result) in cfg.quota.accounts.iter().zip(results) {
        report.push(account_check(cfg, account, result, &interactive, now));
    }
}

pub(super) fn account_check(
    cfg: &Config,
    account: &AccountConfig,
    result: Result<Vec<QuotaSnapshot>, ProbeError>,
    interactive: &[(String, UnixMs)],
    now: UnixMs,
) -> Check {
    let id = format!("ai.{}", account.name);
    let source = account.effective_probe();
    let label = label(source);
    let device = match cfg.agent.ai_accounts.contains(&account.name) {
        true => String::new(),
        false => " (not in agent.ai_accounts: this device cannot spend it)".to_string(),
    };
    match result {
        Ok(snaps) => match probe_verdict(&snaps, now, SNAPSHOT_MAX_AGE_MS) {
            ProbeVerdict::Fresh { age_ms } => {
                let spend = spend_text(cfg, account, &snaps, interactive, now);
                Check::ok(
                    id,
                    format!("{label} {} old, {spend}{device}", rows::dur(age_ms)),
                )
            }
            ProbeVerdict::Stale { age_ms } => Check::warn(
                id,
                format!(
                    "{label} is {} old (limit {}), so the account is not spendable{device}",
                    rows::dur(age_ms),
                    rows::dur(SNAPSHOT_MAX_AGE_MS)
                ),
                refresh(source, &account.name),
            ),
            ProbeVerdict::NoData => Check::warn(
                id,
                format!("{label} has no window data yet{device}"),
                refresh(source, &account.name),
            ),
        },
        Err(e) if not_found(&e) => Check::warn(
            id,
            format!("no {label}: {e}{device}"),
            refresh(source, &account.name),
        ),
        Err(e) => Check::fail(
            id,
            format!("cannot read {label}: {e}{device}"),
            "fix or remove the probe file, or point `probe_path` at it in this account's [[quota.accounts]] entry",
        ),
    }
}

/// The account-level verdict from the data this device holds, with its reasons.
fn spend_text(
    cfg: &Config,
    account: &AccountConfig,
    snaps: &[QuotaSnapshot],
    interactive: &[(String, UnixMs)],
    now: UnixMs,
) -> String {
    let ctx = SpendContext {
        now_ms: now,
        snapshots: snaps,
        interactive_use: interactive,
        ..SpendContext::default()
    };
    let verdicts = quota::views(&cfg.quota, &ctx);
    match verdicts.iter().find(|v| v.account == account.name) {
        Some(v) if v.spendable => format!(
            "spendable now ({})",
            v.reasons.first().map_or("no blocks", String::as_str)
        ),
        Some(v) => format!("not spendable now: {}", v.reasons.join("; ")),
        None => "no verdict".to_string(),
    }
}

fn label(source: ProbeSource) -> &'static str {
    match source {
        ProbeSource::ClaudeStatusline => "Claude statusline capture",
        ProbeSource::CodexSessions => "Codex session data",
        ProbeSource::Manual => "manual quota file",
    }
}

fn refresh(source: ProbeSource, account: &str) -> String {
    match source {
        ProbeSource::ClaudeStatusline => "use Claude Code once so the statusline capture refreshes; if it never appears, install packaging/claude-statusline-capture.sh (README \"Install the statusline capture\")".into(),
        ProbeSource::CodexSessions => "run codex once so a session records its rate limits, or set probe_path to its sessions directory".into(),
        ProbeSource::Manual => format!("write the account's windows to $IDLEMAX_HOME/agent/quota-{account}.json, or set probe_path"),
    }
}

fn not_found(e: &ProbeError) -> bool {
    matches!(e, ProbeError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
}
