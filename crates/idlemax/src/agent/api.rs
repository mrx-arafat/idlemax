//! Agent local API adapter on 127.0.0.1:7879: device status and Global pause, resume and
//! stop-all. It talks only to the agent loop, so it keeps working while the coordinator is down.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use idlemax_core::control::local_paused_after;
use idlemax_core::protocol::{ControlRequest, LocalStatus};
use idlemax_core::types::ControlAction;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use crate::coord::api::http::{self, ApiFailure, BearerToken, JsonBody};
use crate::coord::server::{self, Limits};

/// How long `POST /v1/local/control` waits for the agent loop to apply the action.
const CONTROL_WAIT: Duration = Duration::from_secs(5);

/// The API side of the agent loop's channels.
#[derive(Clone)]
pub struct LocalHandle {
    status: watch::Receiver<LocalStatus>,
    control: mpsc::UnboundedSender<ControlAction>,
}

/// The agent loop side: publish `status` after every change and drain `control` promptly.
#[derive(Debug)]
pub struct LocalPorts {
    pub status: watch::Sender<LocalStatus>,
    pub control: mpsc::UnboundedReceiver<ControlAction>,
}

pub fn local_channel(initial: LocalStatus) -> (LocalHandle, LocalPorts) {
    let (status_tx, status_rx) = watch::channel(initial);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let handle = LocalHandle {
        status: status_rx,
        control: control_tx,
    };
    (
        handle,
        LocalPorts {
            status: status_tx,
            control: control_rx,
        },
    )
}

pub fn router(handle: LocalHandle, token: &str) -> Router {
    let v1 = Router::new()
        .route("/v1/local/status", get(status))
        .route("/v1/local/control", post(control))
        .route_layer(middleware::from_fn_with_state(
            BearerToken::new(token),
            http::require_bearer,
        ));
    Router::new()
        .route("/healthz", get(http::healthz))
        .merge(v1)
        .fallback(http::no_route)
        .method_not_allowed_fallback(http::wrong_method)
        .with_state(handle)
}

/// Serves until the process stops. The caller binds `config.agent.listen` (loopback). It
/// runs on the coordinator's connection handling: header-read and request timeouts, and an
/// accept loop that survives errors, so a silent local client cannot hold a connection forever.
pub async fn serve(
    listener: TcpListener,
    handle: LocalHandle,
    token: String,
) -> std::io::Result<()> {
    serve_limited(listener, handle, token, Limits::default()).await
}

async fn serve_limited(
    listener: TcpListener,
    handle: LocalHandle,
    token: String,
    limits: Limits,
) -> std::io::Result<()> {
    let app = router(handle, &token);
    server::serve_with(listener, app, std::future::pending(), limits).await
}

async fn status(State(h): State<LocalHandle>) -> Json<LocalStatus> {
    Json(h.status.borrow().clone())
}

/// 200 once the agent shows the requested pause state, 202 if it has not within 5 s.
async fn control(
    State(mut h): State<LocalHandle>,
    JsonBody(req): JsonBody<ControlRequest>,
) -> Result<(StatusCode, Json<LocalStatus>), ApiFailure> {
    let expected = local_paused_after(&req).map_err(|e| ApiFailure::BadRequest(e.to_string()))?;
    h.control.send(req.action).map_err(|_| loop_stopped())?;
    let applied = h.status.wait_for(|s| s.local_paused == expected);
    match tokio::time::timeout(CONTROL_WAIT, applied)
        .await
        .map(|r| r.map(|s| s.clone()))
    {
        Ok(Ok(status)) => Ok((StatusCode::OK, Json(status))),
        Ok(Err(_closed)) => Err(loop_stopped()),
        Err(_elapsed) => Ok((StatusCode::ACCEPTED, Json(h.status.borrow().clone()))),
    }
}

fn loop_stopped() -> ApiFailure {
    ApiFailure::Unavailable("the agent loop is not running".into())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// A client that opens a connection and says nothing must be dropped after the header
    /// timeout, like on the coordinator, instead of holding a connection forever.
    #[tokio::test]
    async fn a_silent_client_is_closed_after_the_header_timeout() {
        let (handle, _ports) = local_channel(LocalStatus::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let limits = Limits::new(Duration::from_millis(50), Duration::from_secs(5));
        let server = tokio::spawn(serve_limited(listener, handle, "t".repeat(40), limits));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("the silent client was never dropped");
        assert!(matches!(read, Ok(0) | Err(_)));
        server.abort();
    }
}
