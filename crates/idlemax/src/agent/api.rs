//! Agent local API adapter on 127.0.0.1:7879: device status and Global pause, resume and
//! stop-all. It talks only to the agent loop, so it keeps working while the coordinator is down.

use std::path::Path;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{middleware, Extension, Json, Router};
use idlemax_core::config::ReloadReport;
use idlemax_core::control::caller::{control_access, Access};
use idlemax_core::control::local_paused_after;
use idlemax_core::host::AllowedHosts;
use idlemax_core::protocol::{ControlRequest, LocalStatus};
use idlemax_core::types::ControlAction;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

use crate::coord::api::caller::{CallerCheck, Who};
use crate::coord::api::http::{self, ApiFailure, BearerToken, HostGuard, JsonBody};
use crate::coord::server::{self, Limits};

/// How long `POST /v1/local/control` waits for the agent loop to apply the action.
const CONTROL_WAIT: Duration = Duration::from_secs(5);
/// How long `POST /v1/local/reload` waits. A reload is milliseconds; this stays under the
/// CLI's 15 s request timeout so the agent's own explanation reaches the user.
const RELOAD_WAIT: Duration = Duration::from_secs(12);

/// The API side of the agent loop's channels.
#[derive(Clone)]
pub struct LocalHandle {
    status: watch::Receiver<LocalStatus>,
    control: mpsc::UnboundedSender<LocalControl>,
    reload: mpsc::UnboundedSender<ReloadRequest>,
}

#[derive(Debug)]
pub struct LocalControl {
    pub action: ControlAction,
    pub applied: oneshot::Sender<()>,
}

/// A request to re-read `config.toml`. The loop answers with what it applied, or with why the
/// file could not be used (the agent then keeps running on the settings it has).
#[derive(Debug)]
pub struct ReloadRequest {
    pub reply: oneshot::Sender<Result<ReloadReport, String>>,
}

/// The agent loop side: publish `status` after every change and drain `control` promptly.
#[derive(Debug)]
pub struct LocalPorts {
    pub status: watch::Sender<LocalStatus>,
    pub control: mpsc::UnboundedReceiver<LocalControl>,
    pub reload: mpsc::UnboundedReceiver<ReloadRequest>,
}

pub fn local_channel(initial: LocalStatus) -> (LocalHandle, LocalPorts) {
    let (status_tx, status_rx) = watch::channel(initial);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let (reload_tx, reload_rx) = mpsc::unbounded_channel();
    let handle = LocalHandle {
        status: status_rx,
        control: control_tx,
        reload: reload_tx,
    };
    (
        handle,
        LocalPorts {
            status: status_tx,
            control: control_rx,
            reload: reload_rx,
        },
    )
}

/// `root` is the agent's state dir: a caller launched for it may not resume or reload.
pub fn router(handle: LocalHandle, token: &str, root: &Path) -> Router {
    router_with(handle, token, CallerCheck::for_home(root))
}

fn router_with(handle: LocalHandle, token: &str, check: CallerCheck) -> Router {
    let v1 = Router::new()
        .route("/v1/local/status", get(status))
        .route("/v1/local/control", post(control))
        .route("/v1/local/reload", post(reload))
        .route_layer(middleware::from_fn_with_state(
            BearerToken::new(token),
            http::require_bearer,
        ));
    let guard = HostGuard::new(
        AllowedHosts::default(),
        "the local API answers only to IP addresses and localhost",
    );
    Router::new()
        .route("/healthz", get(http::healthz))
        .merge(v1)
        .fallback(http::no_route)
        .method_not_allowed_fallback(http::wrong_method)
        .with_state(handle)
        .layer(Extension(check))
        .layer(middleware::from_fn_with_state(guard, http::require_host))
}

/// Serves until the process stops. The caller binds `config.agent.listen` (loopback). It
/// runs on the coordinator's connection handling: header-read and request timeouts, and an
/// accept loop that survives errors, so a silent local client cannot hold a connection forever.
pub async fn serve(
    listener: TcpListener,
    handle: LocalHandle,
    token: String,
    root: &Path,
) -> std::io::Result<()> {
    serve_limited(listener, handle, token, root, Limits::default()).await
}

async fn serve_limited(
    listener: TcpListener,
    handle: LocalHandle,
    token: String,
    root: &Path,
    limits: Limits,
) -> std::io::Result<()> {
    let app = router(handle, &token, root);
    server::serve_with(listener, app, std::future::pending(), limits).await
}

async fn status(State(h): State<LocalHandle>) -> Json<LocalStatus> {
    Json(h.status.borrow().clone())
}

/// 200 once the agent has applied this control, 202 if it has not within 5 s. A resume is
/// owner-only.
async fn control(
    State(h): State<LocalHandle>,
    who: Who,
    JsonBody(req): JsonBody<ControlRequest>,
) -> Result<(StatusCode, Json<LocalStatus>), ApiFailure> {
    who.ensure(control_access(req.action)).await?;
    local_paused_after(&req).map_err(|e| ApiFailure::BadRequest(e.to_string()))?;
    let (applied, received) = oneshot::channel();
    h.control
        .send(LocalControl {
            action: req.action,
            applied,
        })
        .map_err(|_| loop_stopped())?;
    match tokio::time::timeout(CONTROL_WAIT, received).await {
        Ok(Ok(())) => Ok((StatusCode::OK, Json(h.status.borrow().clone()))),
        Ok(Err(_closed)) => Err(loop_stopped()),
        Err(_elapsed) => Ok((StatusCode::ACCEPTED, Json(h.status.borrow().clone()))),
    }
}

/// 200 with what the agent applied from `config.toml`, 400 with the reason when the file is
/// not usable (the agent keeps its current settings). Owner-only: it applies settings.
async fn reload(State(h): State<LocalHandle>, who: Who) -> Result<Json<ReloadReport>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let (reply, received) = oneshot::channel();
    h.reload
        .send(ReloadRequest { reply })
        .map_err(|_| loop_stopped())?;
    match tokio::time::timeout(RELOAD_WAIT, received).await {
        Ok(Ok(Ok(report))) => Ok(Json(report)),
        Ok(Ok(Err(why))) => Err(ApiFailure::BadRequest(why)),
        Ok(Err(_closed)) => Err(loop_stopped()),
        Err(_elapsed) => Err(ApiFailure::Unavailable(
            "the agent did not finish reloading in time; check its log and retry".into(),
        )),
    }
}

fn loop_stopped() -> ApiFailure {
    ApiFailure::Unavailable(
        "the agent could not apply this control; check its log and retry".into(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    #[tokio::test]
    async fn reload_answers_with_what_the_loop_applied_or_why_it_could_not() {
        let (handle, mut ports) = local_channel(LocalStatus::default());
        let applied = tokio::spawn(reload(State(handle.clone()), Who::remote()));
        let request = ports.reload.recv().await.unwrap();
        let report = ReloadReport {
            applied: vec!["guard.require_ac_power".into()],
            ..ReloadReport::default()
        };
        request.reply.send(Ok(report.clone())).unwrap();
        let Json(answer) = applied.await.unwrap().unwrap();
        assert_eq!(answer, report);

        let refused = tokio::spawn(reload(State(handle), Who::remote()));
        let request = ports.reload.recv().await.unwrap();
        request.reply.send(Err("config.toml: bad".into())).unwrap();
        let failure = refused.await.unwrap().unwrap_err();
        assert!(matches!(failure, ApiFailure::BadRequest(why) if why == "config.toml: bad"));
    }

    #[tokio::test]
    async fn stop_all_waits_for_this_control_even_when_already_paused() {
        let initial = LocalStatus {
            local_paused: true,
            ..LocalStatus::default()
        };
        let (handle, mut ports) = local_channel(initial.clone());
        let req = ControlRequest {
            scope: idlemax_core::types::ControlScope::Global,
            action: ControlAction::StopAll,
            reason: None,
        };
        let task = tokio::spawn(control(State(handle), Who::remote(), JsonBody(req)));
        let request = ports.control.recv().await.unwrap();
        tokio::task::yield_now().await;
        assert!(
            !task.is_finished(),
            "an old paused status acknowledged a new stop_all"
        );
        ports.status.send_replace(initial);
        request.applied.send(()).unwrap();
        let (code, Json(status)) = task.await.unwrap().unwrap();
        assert_eq!(code, StatusCode::OK);
        assert!(status.local_paused);
    }

    /// The kill switch lives here: a page that reaches it under its own domain name is refused.
    #[tokio::test]
    async fn a_rebinding_host_gets_421_and_loopback_names_do_not() {
        let (handle, _ports) = local_channel(LocalStatus::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, handle, "t".repeat(40), Path::new("/")));
        let get = |host: String| async move {
            reqwest::Client::new()
                .get(format!("http://{addr}/healthz"))
                .header(reqwest::header::HOST, host)
                .send()
                .await
                .unwrap()
                .status()
        };
        assert_eq!(get("evil.example:7879".into()).await, 421);
        assert_eq!(get(format!("127.0.0.1:{}", addr.port())).await, 200);
        assert_eq!(get(format!("localhost:{}", addr.port())).await, 200);
    }

    /// A client that opens a connection and says nothing must be dropped after the header
    /// timeout, like on the coordinator, instead of holding a connection forever.
    #[tokio::test]
    async fn a_silent_client_is_closed_after_the_header_timeout() {
        let (handle, _ports) = local_channel(LocalStatus::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let limits = Limits::new(Duration::from_millis(50), Duration::from_secs(5));
        let server = tokio::spawn(serve_limited(
            listener,
            handle,
            "t".repeat(40),
            Path::new("/"),
            limits,
        ));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("the silent client was never dropped");
        assert!(matches!(read, Ok(0) | Err(_)));
        server.abort();
    }
}
