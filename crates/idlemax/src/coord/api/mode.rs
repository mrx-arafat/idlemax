//! `POST /v1/dispatcher/mode`: switch the dispatcher off or to propose (A1, A3).
//! Adapter: writes `dispatcher.mode` into `config.toml` through the config editor, so the next
//! `live().current()` sees it with no restart. An owner action: never held (A10).

use axum::extract::State;
use axum::Json;
use idlemax_core::config::set_setting;
use idlemax_core::control::caller::Access;
use idlemax_core::dispatch::{Mode, ModeReply, ModeRequest};
use idlemax_core::types::{EventKind, Os};
use idlemax_core::Paths;

use super::caller::Who;
use super::http::{ApiFailure, JsonBody};
use super::AppState;
use crate::coord::store::Store;
use crate::files;
use crate::live_config::LiveConfig;
use crate::state::now_ms;

/// Toward `off` any holder of the token may switch; toward any other mode, even one this
/// build does not know, only the owner.
pub async fn set(
    State(app): State<AppState>,
    who: Who,
    JsonBody(body): JsonBody<serde_json::Value>,
) -> Result<Json<ModeReply>, ApiFailure> {
    let access = if body.get("mode").and_then(|m| m.as_str()) == Some(Mode::Off.as_str()) {
        Access::Stop
    } else {
        Access::Owner
    };
    who.ensure(access).await?;
    let req: ModeRequest = serde_json::from_value(body)
        .map_err(|e| ApiFailure::BadRequest(format!("mode must be \"off\" or \"propose\": {e}")))?;
    let paths = app.paths().clone();
    let live = std::sync::Arc::clone(app.live());
    let reply = app
        .with_store(move |store, _| switch(store, &paths, &live, req.mode))
        .await?;
    Ok(Json(reply))
}

/// One switch, inside the store lock so two switches cannot interleave.
fn switch(
    store: &mut Store,
    paths: &Paths,
    live: &LiveConfig,
    mode: Mode,
) -> Result<ModeReply, ApiFailure> {
    let file = paths.config_file();
    let before_text = files::read_optional(&file)?;
    let text = before_text.clone().unwrap_or_default();
    let edit = set_setting(&text, "dispatcher.mode", mode.as_str(), Os::current())
        .map_err(|e| ApiFailure::BadRequest(e.to_string()))?;
    let before: Mode = serde_json::from_value(edit.before.clone()).unwrap_or_default();
    if edit.changed {
        files::save(&file, before_text.as_deref(), &edit.text)?;
        crate::owner_writes::record(&file, &edit.text);
        let msg = format!("dispatcher mode {before} -> {mode}");
        store.append_event(EventKind::Control, None, None, &msg, now_ms())?;
    }
    // Read back through the live holder: what the next plan and feed reply will see.
    Ok(ModeReply {
        mode: live.current().dispatcher.mode,
        before,
        changed: edit.changed,
    })
}

#[cfg(test)]
mod tests {
    use idlemax_core::protocol::ControlRequest;
    use idlemax_core::types::{ControlAction, ControlScope};
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;

    #[tokio::test]
    async fn the_mode_switch_edits_the_file_once_and_works_while_paused() {
        let api = Api::start().await;
        let file = api.home().join("config.toml");
        let original = "# mine\n[agent]\nnotify = true\n";
        std::fs::write(&file, original).unwrap();
        let pause = ControlRequest {
            action: ControlAction::Pause,
            scope: ControlScope::Global,
            reason: None,
        };
        assert_eq!(
            api.post("/v1/control", &pause).await.status(),
            StatusCode::OK
        );

        let res = api
            .post("/v1/dispatcher/mode", &serde_json::json!({"mode": "off"}))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let reply: ModeReply = res.json().await.unwrap();
        assert_eq!(
            reply,
            ModeReply {
                mode: Mode::Off,
                before: Mode::Propose,
                changed: true
            }
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(original), "{text}");
        assert!(text.contains("[dispatcher]\nmode = \"off\""), "{text}");
        assert!(api.home().join("config.toml.idlemax.bak").exists());

        let again: ModeReply = api
            .post("/v1/dispatcher/mode", &serde_json::json!({"mode": "off"}))
            .await
            .json()
            .await
            .unwrap();
        assert!(!again.changed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

        let unknown = api
            .post("/v1/dispatcher/mode", &serde_json::json!({"mode": "turbo"}))
            .await;
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

        let events: Vec<serde_json::Value> = api
            .get_as("/v1/events", "localhost")
            .await
            .json()
            .await
            .unwrap();
        let switched = events
            .iter()
            .filter(|e| e["message"] == "dispatcher mode propose -> off")
            .count();
        assert_eq!(switched, 1);
    }
}
