//! Adapter: bounded startup probes before setup's final doctor check.

use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use idlemax_core::install::setup::{startup_readiness, Role, StartupReadiness};
use idlemax_core::Paths;
use tokio::time::Instant;

use crate::cli::{ApiClient, ClientError};
use crate::state;

const STARTUP_WAIT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(super) async fn wait(paths: &Paths, role: Role) -> Result<StartupReadiness> {
    let config = state::load_config(paths)?;
    let token = state::load_token(paths)?;
    let url = if role.runs_agent() {
        format!("http://{}", config.agent.listen)
    } else {
        config.coordinator.url
    };
    let client = ApiClient::new(&url, token)?;
    poll(role, STARTUP_WAIT, || async {
        if role.runs_agent() {
            let status = client.local_status().await?;
            Ok((status.coordinator_reachable, status.coordinator_reachable))
        } else {
            client.healthz().await.map(|()| (true, false))
        }
    })
    .await
}

async fn poll<F, Fut>(role: Role, limit: Duration, mut probe: F) -> Result<StartupReadiness>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(bool, bool), ClientError>>,
{
    let started = Instant::now();
    let deadline = started + limit;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(StartupReadiness::Starting);
        }
        let (coord, agent) = match tokio::time::timeout(remaining, probe()).await {
            Ok(Ok(facts)) => facts,
            Ok(Err(ClientError::Unreachable { .. })) => (false, false),
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Ok(StartupReadiness::Starting),
        };
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let budget = u64::try_from(limit.as_millis()).unwrap_or(u64::MAX);
        let state = startup_readiness(role, coord, agent, elapsed, budget);
        if state != StartupReadiness::Waiting {
            return Ok(state);
        }
        tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn polling_waits_for_a_handshake_and_bounds_an_unresponsive_probe() {
        let mut reports = [(true, false), (true, true)].into_iter();
        let result = poll(Role::Both, Duration::from_secs(1), || {
            std::future::ready(Ok(reports.next().unwrap()))
        })
        .await
        .unwrap();
        assert_eq!(result, StartupReadiness::Ready);
        let result = poll(Role::Both, Duration::from_millis(1), || {
            std::future::pending()
        })
        .await
        .unwrap();
        assert_eq!(result, StartupReadiness::Starting);
    }
}
