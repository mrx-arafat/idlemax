//! Adapter: on macOS, asks launchd how each idlemax LaunchAgent is doing, with `launchctl print`.
//! Read-only. It asks only about a LaunchAgent plist under the home directory that declares its
//! own label, so a fixture in a temp home never reaches the real session. Verdicts live in core.

use std::path::Path;
use std::time::Duration;

use idlemax_core::doctor::launchd::{self, Print};
use idlemax_core::doctor::Report;
use tokio::process::Command;

use super::facts::{self, CmdOutcome};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// (kind, label) for each LaunchAgent that `idlemax service install` writes.
const JOBS: [(&str, &str); 2] = [
    ("agent", "com.idlemax.agent"),
    ("coordinator", "com.idlemax.coord"),
];

pub async fn check(report: &mut Report, home: Option<&Path>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(home) = home else {
        return;
    };
    let declared: Vec<(&str, &str, String)> = JOBS
        .into_iter()
        .filter_map(|(kind, label)| {
            let plist = home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            let text = facts::read_text(&plist)?;
            launchd::declares_label(&text, label)
                .then(|| (kind, label, plist.display().to_string()))
        })
        .collect();
    if declared.is_empty() {
        return;
    }
    let uid = user_id().await;
    for (kind, label, plist) in declared {
        let print = match uid {
            Some(uid) => print(uid, label).await,
            None => Print::Unavailable("could not read your user id"),
        };
        report.push(launchd::verdict(kind, label, &plist, print));
    }
}

/// The numeric user id for launchd's `gui/<uid>` domain, read with `id -u` as setup does.
async fn user_id() -> Option<u32> {
    let mut cmd = Command::new("id");
    cmd.arg("-u");
    match facts::output(cmd, PROBE_TIMEOUT).await {
        CmdOutcome::Ran {
            code: Some(0),
            stdout,
            ..
        } => stdout.parse().ok(),
        _ => None,
    }
}

async fn print(uid: u32, label: &str) -> Print {
    let target = format!("gui/{uid}/{label}");
    let mut cmd = Command::new("launchctl");
    cmd.args(["print", target.as_str()]);
    match facts::output(cmd, PROBE_TIMEOUT).await {
        CmdOutcome::Ran {
            code: Some(0),
            stdout,
            ..
        } => Print::Dump(stdout),
        CmdOutcome::Ran { code: Some(_), .. } => Print::NotLoaded,
        _ => Print::Unavailable("launchctl did not answer"),
    }
}
