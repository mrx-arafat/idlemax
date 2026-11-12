//! Adapter: coordinator and local agent reachability, token and version checks.

use std::path::Path;

use idlemax_core::control::credential::ONLY_AGENT_ROUTES;
use idlemax_core::doctor::{version_mismatch, Check, Report};
use idlemax_core::protocol::LocalStatus;
use idlemax_core::{Config, Paths, VERSION};

use super::checks::AFTER_TOKEN;
use crate::cli::{ApiClient, ClientError};

/// Check 3: the coordinator answers, accepts our token and speaks our version.
pub async fn coordinator(
    report: &mut Report,
    cfg: &Config,
    token: Option<&str>,
    flag: Option<&str>,
) {
    let url = flag.unwrap_or(&cfg.coordinator.url);
    let Some(token) = token else {
        return unauthenticated(report, url).await;
    };
    let client = match ApiClient::new(url, token.to_string()) {
        Ok(client) => client,
        Err(e) => {
            report.push(Check::fail(
                "coordinator",
                format!("cannot use {url}: {e}"),
                format!("set a full URL such as http://100.64.0.1:7878 instead of {url}"),
            ));
            return;
        }
    };
    report.push(match client.status().await {
        Ok(status) if version_mismatch(VERSION, &status.version) => Check::warn(
            "coordinator",
            format!("{url} runs {}, this device runs {VERSION}", status.version),
            "install the same idlemax version on the coordinator and every device",
        ),
        Ok(status) => Check::ok(
            "coordinator",
            format!(
                "{url} runs {}, {} device(s) known",
                status.version,
                status.devices.len()
            ),
        ),
        Err(e) => coordinator_error(url, &e),
    });
}

pub(super) fn coordinator_error(url: &str, e: &ClientError) -> Check {
    match e {
        ClientError::Unreachable { .. } => Check::fail(
            "coordinator",
            not_answering(url, e),
            down_fix(url),
        ),
        ClientError::Api { status: 421, .. } => Check::fail(
            "coordinator",
            format!("{url} refused this host name: {}", e.describe()),
            "add the name used in coordinator.url to coordinator.allowed_hosts in the coordinator's config.toml, or put the coordinator's IP address in coordinator.url",
        ),
        ClientError::Api {
            status: 403,
            message,
            ..
        } if message.contains(ONLY_AGENT_ROUTES) => Check::ok(
            "coordinator",
            "this device holds its own credential; owner commands run from a device with the owner token",
        ),
        _ if auth_rejected(e) => Check::fail(
            "coordinator",
            format!("{url} rejected the token: {}", e.describe()),
            "use the same token as the coordinator: `idlemax init --token-stdin` with its token, or set IDLEMAX_TOKEN",
        ),
        _ => Check::fail(
            "coordinator",
            format!("{url} returned an error: {}", e.describe()),
            "run `idlemax status` against it, and read the coordinator log",
        ),
    }
}

/// Reachability without a token: `/healthz` needs no auth, everything else does.
async fn unauthenticated(report: &mut Report, url: &str) {
    let Ok(client) = ApiClient::new(url, String::new()) else {
        report.push(Check::fail(
            "coordinator",
            format!("cannot use {url}"),
            format!("set a full URL such as http://100.64.0.1:7878 instead of {url}"),
        ));
        return;
    };
    report.push(match client.healthz().await {
        Ok(()) => Check::warn(
            "coordinator",
            format!("{url} is up; the token was not checked"),
            AFTER_TOKEN,
        ),
        Err(e) => Check::fail("coordinator", not_answering(url, &e), down_fix(url)),
    });
}

/// "http://host:7878 is not answering: Connection refused (os error 61)": the root
/// cause only, not reqwest's whole chain.
fn not_answering(url: &str, e: &ClientError) -> String {
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    format!("{url} is not answering: {cause}")
}

fn down_fix(url: &str) -> String {
    format!("start it with `idlemax coord`, or point this device at it with `coordinator.url` in config.toml (currently {url})")
}

fn auth_rejected(e: &ClientError) -> bool {
    matches!(
        e,
        ClientError::Api {
            status: 401 | 403,
            ..
        }
    )
}

/// Checks 4 and 4b: the agent on this device answers its local API, and nothing holds its jobs.
pub async fn local_agent(
    report: &mut Report,
    cfg: &Config,
    token: Option<&str>,
    paths: Option<&Paths>,
    agent_unit: bool,
) {
    let url = format!("http://{}", cfg.agent.listen);
    let Some(token) = token else {
        report.push(Check::warn(
            "agent",
            format!("not checked at {url}: no token"),
            AFTER_TOKEN,
        ));
        return;
    };
    let client = match ApiClient::new(&url, token.to_string()) {
        Ok(client) => client,
        Err(e) => {
            report.push(Check::fail(
                "agent",
                format!("cannot use {url}: {e}"),
                "set agent.listen to a loopback address such as 127.0.0.1:7879",
            ));
            return;
        }
    };
    match client.local_status().await {
        Ok(status) => {
            report.push(agent_status_check(&url, &status));
            let file = paths.map_or_else(|| "$IDLEMAX_HOME/PAUSE".into(), Paths::pause_file);
            report.push(pause_check(&status, &file));
        }
        Err(e) => report.push(agent_error(&url, &e, agent_unit)),
    }
}

/// One verdict for a running agent: version first, then whether it reaches the coordinator.
pub(super) fn agent_status_check(url: &str, s: &LocalStatus) -> Check {
    let summary = format!(
        "{url} device {}, agent {}, {} running",
        s.device,
        s.version,
        s.running.len()
    );
    if version_mismatch(VERSION, &s.version) {
        return Check::warn(
            "agent",
            format!("{summary}; this CLI is {VERSION}"),
            "restart the agent so it runs this version: stop it and start `idlemax agent` again",
        );
    }
    if !s.coordinator_reachable {
        return Check::warn(
            "agent",
            format!("{summary}, but it cannot reach the coordinator"),
            "check that the coordinator runs and that coordinator.url and the token match on this device; if the agent just started, run `idlemax doctor` again in a few seconds",
        );
    }
    Check::ok("agent", format!("{summary}, coordinator reachable"))
}

/// Jobs held on this device by the kill switch file or a local pause. Both pause the
/// guard, so no job starts and the running ones are held until the hold is lifted.
pub(super) fn pause_check(s: &LocalStatus, pause_file: &Path) -> Check {
    let file = pause_file.display();
    let remove = format!("rm {file}");
    let resume = "idlemax local resume";
    let (what, fix) = match (s.kill_switch, s.local_paused) {
        (false, false) => return Check::ok("pause", "no kill switch file and no local pause"),
        (true, false) => (format!("the kill switch file {file} exists"), remove),
        (false, true) => ("a local pause is active".to_string(), resume.into()),
        (true, true) => (
            format!("the kill switch file {file} exists and a local pause is active"),
            format!("{remove}, then {resume}"),
        ),
    };
    Check::warn(
        "pause",
        format!("jobs on this device are held: {what}"),
        fix,
    )
}

/// Nothing answers on the agent's address. With the service installed that is a failure, not
/// a hint: the machine should be running jobs and is not.
pub(super) fn agent_down(url: &str, agent_unit: bool) -> Check {
    if !agent_unit {
        return Check::warn(
            "agent",
            format!("no agent answers at {url}"),
            "start it with `idlemax agent`, or install the service (see the service check)",
        );
    }
    let log = if cfg!(target_os = "macos") {
        "<state dir>/logs/agent.log"
    } else {
        "`journalctl --user -u idlemax-agent`"
    };
    Check::fail(
        "agent",
        format!("no agent answers at {url}, though its service is installed"),
        format!("run `idlemax service status`; `idlemax service install` starts it again, and {log} says why it stopped"),
    )
}

fn agent_error(url: &str, e: &ClientError, agent_unit: bool) -> Check {
    match e {
        ClientError::Unreachable { .. } => agent_down(url, agent_unit),
        _ if auth_rejected(e) => Check::fail(
            "agent",
            format!("the agent at {url} rejected the token: {}", e.describe()),
            "use the same token the agent loaded: fix the token file or IDLEMAX_TOKEN",
        ),
        _ => Check::fail(
            "agent",
            format!("{url} returned an error: {}", e.describe()),
            "restart the agent and read its log",
        ),
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::control::credential::refused;
    use idlemax_core::doctor::Level;

    use super::*;

    #[test]
    fn a_device_credential_refused_an_owner_route_is_a_working_coordinator() {
        let api = |status, message: String| ClientError::Api {
            status,
            path: "/v1/status".into(),
            message,
        };
        let held = coordinator_error("http://c", &api(403, refused("box")));
        assert_eq!(held.level, Level::Ok, "{held:?}");
        assert!(held.detail.contains("its own credential"), "{held:?}");
        let other = coordinator_error("http://c", &api(403, "launched".into()));
        assert_eq!(other.level, Level::Fail);
    }
}
