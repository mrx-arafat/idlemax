//! Tests: the coordinator's handling of worker jobs and Runs over real HTTP on loopback.

use reqwest::StatusCode;
use serde_json::json;

use super::tests::Api;

#[tokio::test]
async fn worker_spec_is_refused_from_the_api() {
    let api = Api::start().await;
    let spec = json!({
        "name": "sneaky",
        "command": ["idlemax-worker", "claude"],
        "worker": {"run": 1, "project": "demo"},
    });
    let res = api.post("/v1/jobs", &spec).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body["error"], "worker is set by the dispatcher only",
        "{body}"
    );
}
