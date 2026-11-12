//! HTTP adapter helpers shared by the coordinator API and the agent local API:
//! bearer auth, the JSON error type and a JSON body extractor with JSON rejections.

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use idlemax_core::control::credential::{refused, route_allowed, Identity};
use idlemax_core::host::AllowedHosts;
use idlemax_core::protocol::ApiError;
use serde::de::DeserializeOwned;

/// Every non-2xx response: a status and `{"error": "..."}`.
#[derive(Debug, thiserror::Error)]
pub enum ApiFailure {
    #[error("{0}")]
    BadRequest(String),
    #[error("missing or wrong bearer token")]
    Unauthorized,
    #[error("{0}")]
    NotFound(String),
    #[error("method not allowed")]
    MethodNotAllowed,
    /// The `Host` header names a host this server does not answer to.
    #[error("{0}")]
    Misdirected(String),
    /// The caller may not make this call (an owner-only call from a launched process).
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(String),
    /// An extractor rejection keeps its own status (400, 413, 415, 422).
    #[error("{message}")]
    Rejected { status: StatusCode, message: String },
    #[error("{0}")]
    Unavailable(String),
    /// The detail is logged, never sent to the client.
    #[error("internal error")]
    Internal(String),
}

impl ApiFailure {
    pub fn status(&self) -> StatusCode {
        match self {
            ApiFailure::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiFailure::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiFailure::NotFound(_) => StatusCode::NOT_FOUND,
            ApiFailure::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ApiFailure::Misdirected(_) => StatusCode::MISDIRECTED_REQUEST,
            ApiFailure::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiFailure::Conflict(_) => StatusCode::CONFLICT,
            ApiFailure::Rejected { status, .. } => *status,
            ApiFailure::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiFailure::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        if let ApiFailure::Internal(detail) = &self {
            tracing::error!(%detail, "api request failed");
        }
        let mut res = (self.status(), Json(ApiError::new(self.to_string()))).into_response();
        if matches!(self, ApiFailure::Unauthorized) {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

macro_rules! rejection_into_failure {
    ($($rejection:ty),*) => {$(
        impl From<$rejection> for ApiFailure {
            fn from(r: $rejection) -> Self {
                ApiFailure::Rejected { status: r.status(), message: r.body_text() }
            }
        }
    )*};
}
rejection_into_failure!(JsonRejection, PathRejection, QueryRejection);

/// `axum::Json` whose rejection is an [`ApiFailure`] JSON body instead of plain text.
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiFailure;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state).await?;
        Ok(JsonBody(value))
    }
}

/// Which `Host` values a router answers to (DNS-rebinding defence), and what to tell a
/// caller whose name is refused.
#[derive(Clone)]
pub struct HostGuard {
    hosts: Arc<AllowedHosts>,
    hint: &'static str,
}

impl HostGuard {
    pub fn new(hosts: AllowedHosts, hint: &'static str) -> Self {
        Self {
            hosts: Arc::new(hosts),
            hint,
        }
    }
}

/// Middleware, outermost: 421 unless `Host` is an IP address, a loopback name or a listed name.
/// A request with no `Host` (HTTP/1.0, a plain script) cannot come from a rebinding page.
pub async fn require_host(State(guard): State<HostGuard>, req: Request, next: Next) -> Response {
    if let Some(value) = req.headers().get(header::HOST) {
        let host = value.to_str().unwrap_or("");
        if !guard.hosts.allows(host) {
            let shown: String = String::from_utf8_lossy(value.as_bytes())
                .chars()
                .take(100)
                .collect();
            let message = format!("host {shown:?} is not allowed; {}", guard.hint);
            return ApiFailure::Misdirected(message).into_response();
        }
    }
    next.run(req).await
}

/// The bearer token every `/v1` route requires.
#[derive(Clone)]
pub struct BearerToken(Arc<str>);

impl BearerToken {
    pub fn new(token: &str) -> Self {
        Self(Arc::from(token))
    }

    /// Constant-time compare of an `Authorization` header value; the scheme is case-insensitive.
    fn accepts(&self, authorization: &str) -> bool {
        presented(authorization).is_some_and(|p| constant_time_eq(p, &self.0))
    }
}

/// The bearer of an `Authorization` header value; the scheme is case-insensitive.
fn presented(authorization: &str) -> Option<&str> {
    let (scheme, presented) = authorization.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(presented.trim())
}

/// The SHA-256 digest of a device credential as lowercase hex; the coordinator stores only this.
pub fn credential_digest(credential: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(credential.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The coordinator's auth: the owner token, or a device credential found by its digest.
#[derive(Clone)]
pub struct CoordAccess {
    pub token: BearerToken,
    pub app: super::AppState,
}

/// Middleware of the coordinator's `/v1` routes: the owner token is `Identity::Owner`, a
/// known device credential is `Identity::Device` and opens only the agent routes (A15), and
/// anything else is 401. The identity travels to the handlers as a request extension.
pub async fn require_access(
    State(access): State<CoordAccess>,
    mut req: Request,
    next: Next,
) -> Response {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let identity = if access.token.accepts(header) {
        Identity::Owner
    } else if let Some(bearer) = presented(header).filter(|b| !b.is_empty()) {
        let digest = credential_digest(bearer);
        let found = access
            .app
            .with_store(move |store, _| store.credential_device(&digest).map_err(ApiFailure::from))
            .await;
        match found {
            Ok(Some(device)) => Identity::Device(device),
            Ok(None) => return ApiFailure::Unauthorized.into_response(),
            Err(e) => return e.into_response(),
        }
    } else {
        return ApiFailure::Unauthorized.into_response();
    };
    if let Identity::Device(device) = &identity {
        if !route_allowed(&identity, req.uri().path()) {
            return ApiFailure::Forbidden(refused(device)).into_response();
        }
    }
    req.extensions_mut().insert(identity);
    next.run(req).await
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Middleware: 401 unless the request carries the bearer token.
pub async fn require_bearer(
    State(token): State<BearerToken>,
    req: Request,
    next: Next,
) -> Response {
    let allowed = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| token.accepts(v));
    if !allowed {
        return ApiFailure::Unauthorized.into_response();
    }
    next.run(req).await
}

/// Router fallback for unknown paths.
pub async fn no_route() -> ApiFailure {
    ApiFailure::NotFound("no such route".into())
}

/// Router fallback for a known path with the wrong method.
pub async fn wrong_method() -> ApiFailure {
    ApiFailure::MethodNotAllowed
}

/// `GET /healthz` (no auth).
pub async fn healthz() -> &'static str {
    "ok"
}
