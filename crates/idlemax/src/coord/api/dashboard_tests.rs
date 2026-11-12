//! Tests: every coordinator path the dashboard script calls is a route here, except the ones a
//! later spec adds, which are named. A route the page calls but the router lacks answers 404,
//! and the page shows a red toast for it.

use reqwest::StatusCode;

use super::tests::Api;

const SCRIPT: &str = include_str!("../../../assets/dashboard.js");

/// Each static path prefix in the script and concrete paths that stand for it.
const CALLS: &[(&str, &[&str])] = &[
    ("/v1/advice", &["/v1/advice"]),
    (
        "/v1/advice/",
        &["/v1/advice/1/accept", "/v1/advice/1/dismiss"],
    ),
    ("/v1/advice/refresh", &["/v1/advice/refresh"]),
    ("/v1/control", &["/v1/control"]),
    (
        "/v1/devices/",
        &["/v1/devices/mac/on", "/v1/devices/mac/off"],
    ),
    ("/v1/dispatcher", &["/v1/dispatcher"]),
    ("/v1/dispatcher/mode", &["/v1/dispatcher/mode"]),
    ("/v1/events", &["/v1/events"]),
    ("/v1/jobs", &["/v1/jobs"]),
    ("/v1/jobs/", &["/v1/jobs/1/why", "/v1/jobs/1/cancel"]),
    ("/v1/limits", &["/v1/limits"]),
    (
        "/v1/projects/",
        &["/v1/projects/1/on", "/v1/projects/1/off"],
    ),
    ("/v1/proposals", &["/v1/proposals"]),
    (
        "/v1/proposals/",
        &["/v1/proposals/1/approve", "/v1/proposals/1/reject"],
    ),
    ("/v1/quota", &["/v1/quota"]),
    ("/v1/report", &["/v1/report"]),
    ("/v1/schedules", &["/v1/schedules"]),
    (
        "/v1/schedules/",
        &["/v1/schedules/1", "/v1/schedules/1/pause"],
    ),
    ("/v1/status", &["/v1/status"]),
    ("/v1/why", &["/v1/why"]),
    ("/v1/workers/start", &["/v1/workers/start"]),
    ("/v1/workers/stop", &["/v1/workers/stop"]),
];

/// Called by the page before their route lands: the worker stop and start of dispatcher Spec 2
/// Task 10, and the device switch of Spec 5. The page copes with their 404.
const PENDING: &[&str] = &[
    "/v1/workers/start",
    "/v1/workers/stop",
    "/v1/devices/mac/on",
    "/v1/devices/mac/off",
];

/// The static prefix of every `/v1/` path in the script: up to its first `${`, `?`, quote
/// or other character no path has.
fn prefixes() -> Vec<String> {
    let mut out: Vec<String> = SCRIPT
        .match_indices("/v1/")
        .map(|(at, _)| {
            let rest = &SCRIPT[at..];
            let path = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-');
            rest.chars().take_while(|c| path(*c)).collect::<String>()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[tokio::test]
async fn every_path_the_dashboard_calls_is_a_route_or_named_as_pending() {
    for prefix in prefixes() {
        assert!(
            CALLS.iter().any(|(p, _)| *p == prefix),
            "dashboard.js calls {prefix}: add it to CALLS"
        );
    }
    let api = Api::start().await;
    for path in CALLS.iter().flat_map(|(_, paths)| paths.iter()) {
        let missing = api.get_anon(path).await.status() == StatusCode::NOT_FOUND;
        if PENDING.contains(path) {
            assert!(missing, "{path} landed: take it out of PENDING");
        } else {
            assert!(!missing, "the dashboard calls {path}, which is not a route");
        }
    }
}
