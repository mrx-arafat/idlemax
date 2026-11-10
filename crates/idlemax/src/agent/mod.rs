//! Agent startup: wires config, guard, metrics, runner, local API and the coordinator
//! client into the event loop. Adapter layer.

mod advisor_feed;
pub mod api;
pub mod client;
mod event_loop;
mod keep_awake;
pub mod metrics;
pub mod notify;
mod outbox;
mod probes;
pub mod quota_probe;
mod ring;
pub mod runner;
pub mod supervise;
#[cfg(test)]
mod tests;
mod uplink;

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use idlemax_core::guard::Guard;
use idlemax_core::protocol::LocalStatus;
use idlemax_core::types::{DeviceInfo, Os};
use idlemax_core::{Config, ConfigError, Paths, VERSION};

use crate::state::{self, now_ms, StateError};

use self::client::{ClientError, CoordClient};
use self::event_loop::{Agent, Wiring};
use self::metrics::{MetricsError, Sampler};
use self::outbox::{DurableOutbox, OUTBOX_CAP};
use self::probes::Probes;
use self::ring::{MetricsRing, RingError};
use self::runner::Runner;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("config: {0}")]
    Config(#[from] ConfigError),
    #[error("metrics: {0}")]
    Metrics(#[from] MetricsError),
    #[error("metrics ring: {0}")]
    Ring(#[from] RingError),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("agent.listen {0:?} must be a loopback ip:port")]
    Listen(String),
    #[error("bind local api {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("install signal handler: {0}")]
    Signal(#[source] std::io::Error),
    #[error("startup worker: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

/// Runs the agent until SIGTERM or SIGINT, then stops every job it started.
pub async fn run(paths: Paths, config: Config) -> Result<(), AgentError> {
    state::ensure_dirs(&paths)?;
    let token = state::load_token(&paths)?;
    let addr = loopback_addr(&config.agent.listen)?;
    let client = CoordClient::new(&config.coordinator.url, &token)?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| AgentError::Bind { addr, source })?;
    let (probes, device) = open_probes(&paths, &config).await?;
    let policy = config.guard_policy_for(Os::current(), device.cpu_count, device.mem_total_bytes);
    let guard = Guard::new(policy?);
    let runner = open_runner(&paths, &device, &config).await?;
    let outbox = DurableOutbox::open(&paths.agent_state_dir(), OUTBOX_CAP, now_ms());
    let shutdown = shutdown_signal()?;
    let initial = LocalStatus {
        device: device.id.clone(),
        at_ms: now_ms(),
        version: VERSION.to_string(),
        ..LocalStatus::default()
    };
    let (handle, ports) = api::local_channel(initial);
    tokio::spawn(async move {
        if let Err(e) = api::serve(listener, handle, token).await {
            tracing::error!("local api stopped: {e}");
        }
    });
    tracing::info!(
        device = %device.id,
        coordinator = %config.coordinator.url,
        local_api = %addr,
        "agent started"
    );
    let agent = Agent::new(Wiring {
        device,
        guard,
        client,
        probes,
        runner,
        outbox,
        pause_file: paths.pause_file(),
        status: ports.status,
        sample_every: Duration::from_secs(config.agent.sample_secs),
        heartbeat_every: Duration::from_secs(config.agent.heartbeat_secs),
        advisor_inputs_every: (config.agent.advisor_inputs_secs > 0)
            .then(|| Duration::from_secs(config.agent.advisor_inputs_secs)),
        device_refresh_every: Duration::from_secs(3600),
        notify: config.agent.notify,
        paths: paths.clone(),
        config: config.clone(),
    });
    agent.run(shutdown, ports.control, ports.reload).await;
    tracing::info!("agent stopped");
    Ok(())
}

/// The local API carries the kill switch, so it never listens beyond loopback.
fn loopback_addr(listen: &str) -> Result<SocketAddr, AgentError> {
    listen
        .parse::<SocketAddr>()
        .ok()
        .filter(|a| a.ip().is_loopback())
        .ok_or_else(|| AgentError::Listen(listen.to_string()))
}

async fn open_probes(paths: &Paths, config: &Config) -> Result<(Probes, DeviceInfo), AgentError> {
    let (paths, agent_cfg, quota) = (paths.clone(), config.agent.clone(), config.quota.clone());
    tokio::task::spawn_blocking(move || {
        let ring = MetricsRing::open(&paths.agent_state_dir(), agent_cfg.sample_secs)?;
        let mut probes = Probes::new(Sampler::new()?, ring, &paths, quota, agent_cfg.clone());
        let device = probes.device_info(&agent_cfg);
        Ok::<_, AgentError>((probes, device))
    })
    .await?
}

/// `Runner::new` kills job groups orphaned by a previous agent, so it blocks.
async fn open_runner(
    paths: &Paths,
    device: &DeviceInfo,
    config: &Config,
) -> Result<Runner, AgentError> {
    let (paths, id, agent_cfg) = (paths.clone(), device.id.clone(), config.agent.clone());
    Ok(tokio::task::spawn_blocking(move || Runner::new(paths, id, agent_cfg)).await?)
}

#[cfg(unix)]
fn shutdown_signal() -> Result<impl Future<Output = ()>, AgentError> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).map_err(AgentError::Signal)?;
    let mut int = signal(SignalKind::interrupt()).map_err(AgentError::Signal)?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    })
}

// SHORTCUT: Ctrl-C only on Windows, add service stop events when a Windows device joins.
#[cfg(not(unix))]
fn shutdown_signal() -> Result<impl Future<Output = ()>, AgentError> {
    Ok(async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    })
}
