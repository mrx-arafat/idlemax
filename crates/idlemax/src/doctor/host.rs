//! Adapter: the allowlist, disk and `gh` checks, the facts about this host that jobs depend on.

use std::path::Path;
use std::time::Duration;

use idlemax_core::doctor::{self, format_mib, Check, DiskVerdict, Report};
use idlemax_core::types::Os;
use idlemax_core::Config;

use super::facts;
use crate::cli::rows;

const GH_TIMEOUT: Duration = Duration::from_secs(10);

/// Check 5: every allowlisted program can be found and started.
pub fn allowlist(report: &mut Report, cfg: &Config) {
    let entries = &cfg.agent.command_allowlist;
    if entries.is_empty() {
        report.push(Check::warn(
            "allowlist",
            "agent.command_allowlist is empty: no job can start",
            "list the programs jobs may run, e.g. [\"git\", \"/usr/bin/make\"]",
        ));
        return;
    }
    let mut problems = Vec::new();
    let mut steps = Vec::new();
    for entry in entries {
        if let Some((problem, step)) = entry_problem(entry) {
            problems.push(problem);
            steps.push(step);
        }
    }
    if problems.is_empty() {
        report.push(Check::ok(
            "allowlist",
            format!("{} of {} programs resolve", entries.len(), entries.len()),
        ));
        return;
    }
    report.push(Check::fail(
        "allowlist",
        format!(
            "{} of {} entries will not run: {}",
            problems.len(),
            entries.len(),
            problems.join(", ")
        ),
        format!(
            "{}, or remove the failing entries from agent.command_allowlist",
            steps.join("; ")
        ),
    ));
}

/// What is wrong with one allowlist entry, and the step that fixes it.
fn entry_problem(entry: &str) -> Option<(String, String)> {
    let path = Path::new(entry);
    if path.is_absolute() {
        return match (path.is_file(), facts::is_executable(path)) {
            (false, _) => Some((format!("{entry} (missing)"), format!("install {entry}"))),
            (true, false) => Some((
                format!("{entry} (not executable)"),
                format!("chmod +x {entry}"),
            )),
            (true, true) => None,
        };
    }
    if entry.contains('/') {
        return Some((
            format!("{entry} (relative path)"),
            format!("write {entry} as an absolute path"),
        ));
    }
    match facts::which(entry) {
        Some(_) => None,
        None => Some((
            format!("{entry} (not on PATH)"),
            format!("install {entry} on the agent's PATH"),
        )),
    }
}

/// Check 6: free disk space against the guard's pause and kill lines.
pub async fn disk(report: &mut Report, cfg: &Config) {
    let policy = match cfg.guard_policy(Os::current()) {
        Ok(policy) => policy,
        Err(e) => {
            report.push(Check::fail(
                "disk",
                format!("cannot read the guard policy: {e}"),
                "fix the [guard] overrides in config.toml",
            ));
            return;
        }
    };
    let free = tokio::task::spawn_blocking(facts::disk_free_mib)
        .await
        .ok()
        .flatten();
    report.push(match free {
        Some(free) => disk_check(free, policy.disk_free_mib_pause, policy.disk_free_mib_kill),
        None => Check::warn(
            "disk",
            "cannot read free space on /",
            "check that `df -kP /` works",
        ),
    });
}

fn disk_check(free: u64, pause: Option<u64>, kill: Option<u64>) -> Check {
    let tidy = "delete old job logs under $IDLEMAX_HOME/logs, build directories or caches";
    let missing = |line: u64| format_mib(line.saturating_sub(free));
    match doctor::disk_verdict(free, pause, kill) {
        None => Check::ok(
            "disk",
            format!("free {}; the profile sets no disk line", format_mib(free)),
        ),
        Some(DiskVerdict::Ok) => Check::ok(
            "disk",
            format!("free {}, {}", format_mib(free), lines_text(pause, kill)),
        ),
        Some(DiskVerdict::BelowPause { line_mib, .. }) => Check::warn(
            "disk",
            format!(
                "free {} is below the pause line {}: jobs will never start",
                format_mib(free),
                format_mib(line_mib)
            ),
            format!("free at least {} ({tidy})", missing(line_mib)),
        ),
        Some(DiskVerdict::BelowKill { line_mib, .. }) => Check::fail(
            "disk",
            format!(
                "free {} is below the kill line {}: running jobs are killed",
                format_mib(free),
                format_mib(line_mib)
            ),
            format!("free at least {} now ({tidy})", missing(line_mib)),
        ),
    }
}

/// "pause line 10.0 GiB, kill line 5.0 GiB", naming only the lines that are set.
fn lines_text(pause: Option<u64>, kill: Option<u64>) -> String {
    [("pause", pause), ("kill", kill)]
        .iter()
        .filter_map(|(name, mib)| mib.map(|mib| format!("{name} line {}", format_mib(mib))))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check 8: the advisor's read-only `gh` access.
pub async fn github(report: &mut Report, cfg: &Config) {
    if !cfg.advisor.enabled {
        report.push(Check::ok("gh", "advisor disabled: no GitHub input needed"));
        return;
    }
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["auth", "status"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
    report.push(match facts::output(cmd, GH_TIMEOUT).await {
        facts::CmdOutcome::Missing => Check::warn(
            "gh",
            "gh is not installed: the advisor has no PR or issue input",
            "install the GitHub CLI (https://cli.github.com), then run `gh auth login`",
        ),
        facts::CmdOutcome::Timeout => Check::warn(
            "gh",
            format!(
                "`gh auth status` did not answer within {} s",
                GH_TIMEOUT.as_secs()
            ),
            "run `gh auth status` yourself: a stuck gh stalls the advisor",
        ),
        facts::CmdOutcome::Spawn(e) => Check::warn(
            "gh",
            format!("cannot run gh: {e}"),
            "check that `gh` is on the PATH the agent runs with",
        ),
        facts::CmdOutcome::Ran { code: Some(0), .. } => {
            Check::ok("gh", "gh is installed and authenticated")
        }
        facts::CmdOutcome::Ran { stderr, .. } => {
            let reason = stderr.lines().next().unwrap_or("no output from gh");
            Check::warn(
                "gh",
                format!("gh is not authenticated: {}", rows::trunc(reason, 160)),
                "run `gh auth login`: the advisor only reads your open PRs and issues",
            )
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_check_names_the_shortfall_and_only_the_lines_that_are_set() {
        let below_pause = disk_check(900, Some(1000), Some(500));
        assert_eq!(below_pause.level, doctor::Level::Warn);
        assert!(below_pause.detail.contains("jobs will never start"));
        assert!(below_pause.fix.unwrap().contains("free at least 100 MiB"));

        let below_kill = disk_check(400, Some(1000), Some(500));
        assert_eq!(below_kill.level, doctor::Level::Fail);
        assert!(below_kill
            .fix
            .unwrap()
            .contains("free at least 100 MiB now"));

        let ok = disk_check(2048, None, Some(512));
        assert_eq!(ok.level, doctor::Level::Ok);
        assert_eq!(ok.detail, "free 2.0 GiB, kill line 512 MiB");
    }
}
