//! Web dashboard (delivery layer): serves the embedded single-file page at `/`.
//!
//! The page holds no secrets; it asks for the bearer token and calls the `/v1` API with it.

use axum::http::header;
use axum::response::{Html, IntoResponse};

/// The whole dashboard: HTML, CSS and vanilla JS in one file, no remote resources.
pub const INDEX_HTML: &str = include_str!("../../assets/dashboard.html");

/// Same-origin API calls only; the inline script and style are the only code the page runs.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
                   frame-ancestors 'none'";

/// GET / (no auth).
pub async fn index() -> impl IntoResponse {
    (
        [
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Html(INDEX_HTML),
    )
}
