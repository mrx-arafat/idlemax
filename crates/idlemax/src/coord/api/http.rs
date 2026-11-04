//! HTTP adapter helpers shared by the coordinator API and the agent local API:
//! bearer auth, the JSON error type and a JSON body extractor with JSON rejections.

use std::sync::Arc;

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
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

/// The bearer token every `/v1` route requires.
#[derive(Clone)]
pub struct BearerToken(Arc<str>);

impl BearerToken {
    pub fn new(token: &str) -> Self {
        Self(Arc::from(token))
    }

    /// Constant-time compare of an `Authorization` header value; the scheme is case-insensitive.
    fn accepts(&self, authorization: &str) -> bool {
        let Some((scheme, presented)) = authorization.split_once(' ') else {
            return false;
        };
        scheme.eq_ignore_ascii_case("bearer") && constant_time_eq(presented.trim(), &self.0)
    }
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
