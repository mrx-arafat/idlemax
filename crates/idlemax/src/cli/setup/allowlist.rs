//! Adapter: prepare and persist setup's explicit allowlist edit without losing comments.

use anyhow::Result;
use idlemax_core::config::{set_setting, show_value, Edit};
use idlemax_core::install::quote::shell_quote;
use idlemax_core::install::setup::{ConfigPlan, Role, SetupPlan};
use idlemax_core::types::Os;
use idlemax_core::{Config, Paths};

use super::report::{Status, StepReport};
use crate::cli::{files, ApiClient, CliError, ClientError};
use crate::state;

pub(super) const NEXT: &str = "idlemax config set agent.command_allowlist git,cargo";

/// Validate before any setup step writes; the original file is the editor's source.
pub(super) fn prepare(
    paths: &Paths,
    plan: &SetupPlan,
    raw: Option<&str>,
) -> Result<(Option<Edit>, bool)> {
    let text = match &plan.config {
        ConfigPlan::Write(config) => config.to_toml_string()?,
        ConfigPlan::Keep => std::fs::read_to_string(paths.config_file())?,
    };
    let edit = raw
        .map(|value| set_setting(&text, "agent.command_allowlist", value, Os::current()))
        .transpose()
        .map_err(|e| CliError::usage(e.to_string(), "idlemax setup --help"))?;
    let config = Config::parse(edit.as_ref().map_or(text.as_str(), |e| &e.text))?;
    Ok((
        edit,
        plan.role.runs_agent() && config.agent.command_allowlist.is_empty(),
    ))
}

pub(super) fn step(paths: &Paths, edit: &Edit, dry_run: bool) -> StepReport {
    let value = show_value(&edit.after);
    let command = format!(
        "idlemax config set agent.command_allowlist {}",
        shell_quote(&value)
    );
    let detail = format!("agent.command_allowlist = {value}");
    if !edit.changed {
        return StepReport::new("allowlist", Status::AlreadyDone, detail, command);
    }
    let file = paths.config_file();
    if dry_run {
        return StepReport::new("allowlist", Status::WouldDo, detail, command)
            .with_files(vec![file]);
    }
    let saved = files::read_optional(&file)
        .and_then(|before| files::save(&file, before.as_deref(), &edit.text))
        .inspect(|_| crate::owner_writes::record(&file, &edit.text));
    match saved {
        Ok(backup) => StepReport::new("allowlist", Status::Done, detail, command)
            .with_files(vec![file])
            .with_notes(
                backup
                    .map(|p| format!("backup: {}", p.display()))
                    .into_iter()
                    .collect(),
            ),
        Err(e) => StepReport::new("allowlist", Status::Failed, e.to_string(), command),
    }
}

/// Existing services need live reload; a new service reads the edited file on startup.
async fn reload(paths: &Paths) -> Result<()> {
    let config = state::load_config(paths)?;
    let client = ApiClient::new(
        &format!("http://{}", config.agent.listen),
        state::load_token(paths)?,
    )?;
    match client.local_reload().await {
        Ok(_) | Err(ClientError::Unreachable { .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub(super) async fn reload_existing(
    paths: &Paths,
    role: Role,
    dry_run: bool,
    steps: &mut [StepReport],
) {
    if dry_run
        || !role.runs_agent()
        || steps.iter().any(|s| {
            s.status == Status::Failed
                || (s.step == "service" && s.which == Some("agent") && s.status == Status::Done)
        })
    {
        return;
    }
    let Some(step) = steps
        .iter_mut()
        .find(|s| s.step == "allowlist" && matches!(s.status, Status::Done | Status::AlreadyDone))
    else {
        return;
    };
    if let Err(error) = reload(paths).await {
        step.status = Status::Failed;
        step.detail = format!("saved, but agent reload failed: {error}; run idlemax doctor");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_refused_live_reload_is_reported_as_saved_but_failed() {
        use axum::{http::StatusCode, routing::post, Json, Router};
        use idlemax_core::protocol::ApiError;

        let root = tempfile::tempdir().unwrap();
        let paths = Paths::new(root.path().join("state"));
        crate::cli::init::init(&paths, &crate::cli::init::InitOptions::default()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = state::load_config(&paths).unwrap();
        config.agent.listen = listener.local_addr().unwrap().to_string();
        std::fs::write(paths.config_file(), config.to_toml_string().unwrap()).unwrap();
        let app = Router::new().route(
            "/v1/local/reload",
            post(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ApiError::new("reload refused")),
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        for status in [Status::Done, Status::AlreadyDone] {
            let mut steps = vec![StepReport::new("allowlist", status, "saved", NEXT.into())];
            reload_existing(&paths, Role::Both, false, &mut steps).await;
            assert_eq!(
                steps[0].status,
                Status::Failed,
                "a repeated --allow must retry a refused reload"
            );
            assert!(steps[0].detail.contains("saved, but agent reload failed"));
            assert!(steps[0].detail.contains("reload refused"));
            assert!(steps[0].detail.contains("idlemax doctor"));
        }
        server.abort();
        let _ = server.await;
    }
}
