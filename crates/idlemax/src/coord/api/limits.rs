//! `GET` and `POST /v1/limits`: read and change the owner's hard limits (D18.1, A2).
//! Adapter: edits `config.toml` with the same editor as `idlemax config set` and calls no
//! daemon, because each daemon re-reads these keys when the file's mtime changes. Changing a
//! limit is owner-only (the caller check of Task 4); no pause or guard holds it (A10).

use axum::extract::State;
use axum::Json;
use idlemax_core::config::{
    limit_settings, plan_reload, set_limit, Config, LimitError, LimitReply, LimitRequest,
    ReloadReport, Setting,
};
use idlemax_core::control::caller::Access;
use idlemax_core::types::Os;
use idlemax_core::Paths;

use super::caller::Who;
use super::http::{ApiFailure, JsonBody};
use super::AppState;
use crate::files;

/// `GET /v1/limits`: each limit with its value, default and whether the file sets it.
pub async fn list(State(app): State<AppState>) -> Result<Json<Vec<Setting>>, ApiFailure> {
    let file = app.paths().config_file();
    let limits = app
        .with_store(move |_, _| {
            let text = files::read_optional(&file)?.unwrap_or_default();
            limit_settings(&text, Os::current()).map_err(|e| {
                ApiFailure::Unavailable(format!(
                    "config.toml is not valid: {e}; fix it by hand or run `idlemax doctor`"
                ))
            })
        })
        .await?;
    Ok(Json(limits))
}

/// `POST /v1/limits`: one limit set, or back to its default with `value: null`.
pub async fn set(
    State(app): State<AppState>,
    who: Who,
    JsonBody(req): JsonBody<LimitRequest>,
) -> Result<Json<LimitReply>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let paths = app.paths().clone();
    // Inside the store lock, so a limit and a mode switch never write the file at once.
    let reply = app.with_store(move |_, _| change(&paths, req)).await?;
    Ok(Json(reply))
}

fn change(paths: &Paths, req: LimitRequest) -> Result<LimitReply, ApiFailure> {
    let file = paths.config_file();
    let before_text = files::read_optional(&file)?;
    let text = before_text.clone().unwrap_or_default();
    let edit = set_limit(&text, &req.key, req.value.as_deref(), Os::current())
        .map_err(|e: LimitError| ApiFailure::BadRequest(e.to_string()))?;
    let report = if edit.changed {
        files::save(&file, before_text.as_deref(), &edit.text)?;
        let parsed = |t: &str| Config::parse(t).map_err(|e| ApiFailure::Internal(e.to_string()));
        plan_reload(&parsed(&text)?, &parsed(&edit.text)?)
            .map_err(|e| ApiFailure::Internal(e.to_string()))?
    } else {
        ReloadReport::default()
    };
    Ok(LimitReply {
        key: req.key,
        before: edit.before,
        after: edit.after,
        changed: edit.changed,
        report,
    })
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::json;

    use super::super::tests::Api;
    use super::*;

    #[tokio::test]
    async fn a_limit_is_saved_with_a_backup_reported_live_and_unset_and_others_are_refused() {
        let api = Api::start().await;
        let file = api.home().join("config.toml");
        let original = "# mine\n[agent]\nnotify = true\n";
        std::fs::write(&file, original).unwrap();
        let key = "dispatcher.hold_mem_used_pct";

        let res = api
            .post("/v1/limits", &json!({"key": key, "value": "85"}))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let reply: LimitReply = res.json().await.unwrap();
        assert!(reply.changed);
        assert_eq!(reply.report.applied, [key]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# mine\n") && text.contains("hold_mem_used_pct = 85"));
        let backup = files::backup_path(&file);
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);

        let res = api
            .post("/v1/limits", &json!({"key": key, "value": null}))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!std::fs::read_to_string(&file).unwrap().contains(key));

        let before = std::fs::read_to_string(&file).unwrap();
        for key in ["agent.command_allowlist", "dispatcher.mode"] {
            let res = api
                .post("/v1/limits", &json!({"key": key, "value": "x"}))
                .await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{key}");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }
}
