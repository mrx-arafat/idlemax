//! Web dashboard (delivery layer): serves the embedded page at `/` and its one script at
//! `/dashboard.js`.
//!
//! The page holds no secrets; it asks for the bearer token and calls the `/v1` API with it.
//! The script is a separate file so the policy can forbid inline script altogether.

use axum::http::header;
use axum::response::{Html, IntoResponse};

/// The page: HTML and CSS, no remote resources.
pub const INDEX_HTML: &str = include_str!("../../assets/dashboard.html");

/// The dashboard's vanilla JS, the only script the page runs.
pub const SCRIPT_JS: &str = include_str!("../../assets/dashboard.js");

/// Only the script served from this origin runs, and the page may call only this origin. Styles
/// stay inline (the page sets a few `style` attributes); style cannot run code, and with no
/// remote image, font or connection allowed it cannot send anything out either.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; \
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

/// GET /dashboard.js (no auth: it is code, not data).
pub async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        SCRIPT_JS,
    )
}
