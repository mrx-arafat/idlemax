//! Adapter: coordinator and local agent reachability, token and version checks.

use idlemax_core::doctor::{version_mismatch, Check, Report};
use idlemax_core::protocol::LocalStatus;
use idlemax_core::{Config, VERSION};

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

fn coordinator_error(url: &str, e: &ClientError) -> Check {
    match e {
        ClientError::Unreachable { .. } => Check::fail(
            "coordinator",
            not_answering(url, e),
            down_fix(url),
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

/// Check 4: the agent on this device answers its local API.
pub async fn local_agent(report: &mut Report, cfg: &Config, token: Option<&str>) {
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
        Ok(status) => report.push(agent_status_check(&url, &status)),
        Err(e) => report.push(agent_error(&url, &e)),
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

fn agent_error(url: &str, e: &ClientError) -> Check {
    match e {
        ClientError::Unreachable { .. } => Check::warn(
            "agent",
            format!("no agent answers at {url}"),
            "start it with `idlemax agent`, or install the service (see the service check)",
        ),
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
