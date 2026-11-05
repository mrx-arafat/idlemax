//! Coordinator HTTP server adapter: HTTP/1 connections, explicit timeouts and graceful stop.

use std::future::Future;
use std::io;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tower_http::timeout::TimeoutLayer;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy)]
struct Limits {
    header_read: Duration,
    request: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            header_read: HEADER_READ_TIMEOUT,
            request: REQUEST_TIMEOUT,
        }
    }
}

/// Serves until `shutdown` resolves, then drains connections.
pub async fn serve<F>(listener: TcpListener, app: Router, shutdown: F) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    serve_with(listener, app, shutdown, Limits::default()).await
}

async fn serve_with<F>(
    listener: TcpListener,
    app: Router,
    shutdown: F,
    limits: Limits,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let app = app.layer(TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        limits.request,
    ));
    let (close_tx, close_rx) = watch::channel(());
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                connections.spawn(connection(stream, app.clone(), close_rx.clone(), limits));
            }
            Some(joined) = connections.join_next() => {
                if let Err(e) = joined {
                    tracing::warn!(error = %e, "connection task failed");
                }
            }
        }
    }
    drop(close_tx);
    drop(listener);
    while let Some(joined) = connections.join_next().await {
        if let Err(e) = joined {
            tracing::warn!(error = %e, "connection task failed during shutdown");
        }
    }
    Ok(())
}

async fn connection(
    stream: TcpStream,
    app: Router,
    mut close: watch::Receiver<()>,
    limits: Limits,
) {
    let mut builder = auto::Builder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .header_read_timeout(limits.header_read)
        .timer(TokioTimer::new());
    let connection = builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app));
    tokio::pin!(connection);
    tokio::select! {
        result = connection.as_mut() => {
            if let Err(e) = result {
                tracing::debug!(error = %e, "connection closed with error");
            }
        }
        _ = close.changed() => {
            connection.as_mut().graceful_shutdown();
            if let Err(e) = connection.await {
                tracing::debug!(error = %e, "connection closed with error during shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::routing::get;
    use axum::Router;
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::{serve_with, Limits};

    const SHORT_HEADER: Duration = Duration::from_millis(50);
    const SHORT_REQUEST: Duration = Duration::from_millis(50);

    fn short_limits() -> Limits {
        Limits {
            header_read: SHORT_HEADER,
            request: SHORT_REQUEST,
        }
    }

    #[tokio::test]
    async fn silent_client_is_closed_after_header_timeout() {
        let (addr, stop, server) = start(Router::new()).await;
        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut byte = [0_u8; 1];
        let read = timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("silent client was not dropped");
        assert!(matches!(read, Ok(0) | Err(_)));
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn slow_handler_gets_request_timeout() {
        let app = Router::new().route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "late"
            }),
        );
        let (addr, stop, server) = start(app).await;
        let response = reqwest::get(format!("http://{addr}/slow")).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::REQUEST_TIMEOUT);
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }

    async fn start(
        app: Router,
    ) -> (
        std::net::SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = oneshot::channel();
        let server = tokio::spawn(serve_with(
            listener,
            app,
            async move {
                let _ = stop_rx.await;
            },
            short_limits(),
        ));
        (addr, stop_tx, server)
    }
}
