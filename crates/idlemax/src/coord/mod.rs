//! Coordinator startup adapter (`idlemax coord`): opens the store, serves the HTTP API,
//! runs the background sweeps, and stops cleanly on SIGINT or SIGTERM.

pub mod api;
mod dispatch;
mod schedule;
pub(crate) mod server;
pub mod store;
mod sweep;
pub mod web;

use std::future::Future;
use std::net::SocketAddr;

use idlemax_core::{Config, Paths};
use tokio::net::TcpListener;

use self::api::AppState;
use self::store::{Store, StoreError};
use crate::state::{self, StateError};

#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("store task: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("coordinator.listen {0:?} is not an ip:port address")]
    BadListen(String),
    #[error("refusing to listen on {0}: use a loopback or tailnet address, never all interfaces")]
    AllInterfaces(SocketAddr),
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("signal handler: {0}")]
    Signal(#[source] std::io::Error),
    #[error("serve: {0}")]
    Serve(#[source] std::io::Error),
}

/// Runs until SIGINT or SIGTERM. Startup failures are returned, never retried.
pub async fn run(paths: Paths, config: Config) -> Result<(), CoordError> {
    let addr = listen_addr(&config.coordinator.listen)?;
    let token = state::load_token(&paths)?;
    state::ensure_dirs(&paths)?;
    if let Err(e) = crate::dispatch::rules_file::ensure(&paths) {
        tracing::warn!(error = %e, "cannot write rules.md; passes use the default rules");
    }
    let db = paths.db_file();
    let store = tokio::task::spawn_blocking(move || Store::open(&db)).await??;
    let shutdown = shutdown_signal()?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| CoordError::Bind { addr, source })?;
    let app = AppState::new(store, config, paths);
    let sweeps = sweep::spawn(app.clone());
    tracing::info!(%addr, "coordinator listening");
    let served = server::serve(listener, api::router(app, &token), shutdown).await;
    sweeps.iter().for_each(|t| t.abort());
    tracing::info!("coordinator stopped");
    served.map_err(CoordError::Serve)
}

/// The coordinator holds the job queue and runs commands on devices, so it must never be
/// reachable on every interface: loopback by default, the tailnet IP in production.
fn listen_addr(listen: &str) -> Result<SocketAddr, CoordError> {
    let addr: SocketAddr = listen
        .parse()
        .map_err(|_| CoordError::BadListen(listen.into()))?;
    // `::ffff:0.0.0.0` is the IPv4 wildcard in IPv6 clothes: look through the mapping.
    if addr.ip().to_canonical().is_unspecified() {
        return Err(CoordError::AllInterfaces(addr));
    }
    Ok(addr)
}

/// Resolves on the first SIGINT or SIGTERM. Handlers are installed before serving starts.
fn shutdown_signal() -> Result<impl Future<Output = ()>, CoordError> {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(CoordError::Signal)?;
    Ok(async move {
        #[cfg(unix)]
        tokio::select! {
            Ok(()) = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        #[cfg(not(unix))]
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
        tracing::info!("shutdown signal received");
    })
}

#[cfg(test)]
mod tests {
    use super::{listen_addr, CoordError};

    #[test]
    fn every_spelling_of_all_interfaces_is_refused() {
        for wildcard in ["0.0.0.0:7878", "[::]:7878", "[::ffff:0.0.0.0]:7878"] {
            assert!(
                matches!(listen_addr(wildcard), Err(CoordError::AllInterfaces(_))),
                "{wildcard} must be refused"
            );
        }
    }

    #[test]
    fn a_single_interface_is_accepted() {
        for one in [
            "127.0.0.1:7878",
            "[::1]:7878",
            "100.64.0.1:7878",
            "[::ffff:127.0.0.1]:7878",
        ] {
            assert!(listen_addr(one).is_ok(), "{one} must be accepted");
        }
        assert!(matches!(
            listen_addr("localhost:7878"),
            Err(CoordError::BadListen(_))
        ));
    }
}
