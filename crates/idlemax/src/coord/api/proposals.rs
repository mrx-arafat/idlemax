//! `GET /v1/proposals` and `POST /v1/proposals/{id}/approve|reject` (D18.4). Adapter: an
//! approval appends the line to `rules.md` with the pure `append_rule`, then marks the row;
//! nothing reaches the file without it. Deciding is owner-only.

use axum::extract::{Path, State};
use axum::Json;
use idlemax_core::control::caller::Access;
use idlemax_core::dispatch::proposal::{append_rule, Proposal, ProposalStatus};
use idlemax_core::Paths;

use super::caller::Who;
use super::http::ApiFailure;
use super::AppState;
use crate::coord::store::Store;
use crate::dispatch::rules_file::MAX_RULES_BYTES;
use crate::files;
use crate::state::now_ms;

pub async fn list(State(app): State<AppState>) -> Result<Json<Vec<Proposal>>, ApiFailure> {
    let all = app
        .with_store(|store, _| Ok::<_, ApiFailure>(store.proposals()?))
        .await?;
    Ok(Json(all))
}

pub async fn approve(
    State(app): State<AppState>,
    who: Who,
    Path(id): Path<i64>,
) -> Result<Json<Proposal>, ApiFailure> {
    decide(app, who, id, ProposalStatus::Approved).await
}

pub async fn reject(
    State(app): State<AppState>,
    who: Who,
    Path(id): Path<i64>,
) -> Result<Json<Proposal>, ApiFailure> {
    decide(app, who, id, ProposalStatus::Rejected).await
}

async fn decide(
    app: AppState,
    who: Who,
    id: i64,
    status: ProposalStatus,
) -> Result<Json<Proposal>, ApiFailure> {
    who.ensure(Access::Owner).await?;
    let paths = app.paths().clone();
    let now = now_ms();
    let decided = app
        .with_store(move |store, _| {
            let p = open_proposal(store, id)?;
            if status == ProposalStatus::Approved {
                add_line(&paths, &p.line)?;
            }
            store.decide_proposal(id, status, now)?;
            Ok::<_, ApiFailure>(store.proposal(id)?.unwrap_or(p))
        })
        .await?;
    Ok(Json(decided))
}

fn open_proposal(store: &Store, id: i64) -> Result<Proposal, ApiFailure> {
    let p = store
        .proposal(id)?
        .ok_or_else(|| ApiFailure::NotFound(format!("proposal {id} not found")))?;
    if p.status != ProposalStatus::Open {
        return Err(ApiFailure::Conflict(format!(
            "proposal {id} is already {}",
            p.status.as_str()
        )));
    }
    Ok(p)
}

/// Appends the line to `rules.md`. A line already there changes nothing, so an approve whose
/// row update failed only marks the row the next time.
fn add_line(paths: &Paths, line: &str) -> Result<(), ApiFailure> {
    let file = paths.rules_file();
    let before = files::read_optional(&file)?;
    let text = before.clone().unwrap_or_default();
    let new = append_rule(&text, line);
    if new == text {
        return Ok(());
    }
    // A line past the clip of `rules_file::read` would never reach a pass.
    if new.len() > MAX_RULES_BYTES {
        return Err(ApiFailure::Conflict(
            "rules.md is at its 16 KiB limit: shorten it, then approve".into(),
        ));
    }
    files::save(&file, before.as_deref(), &new)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use idlemax_core::dispatch::proposal::RuleProposal;
    use idlemax_core::Config;
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;

    #[tokio::test]
    async fn approve_appends_once_reject_leaves_the_file_and_the_list_shows_open_first() {
        let mut store = Store::open_in_memory().unwrap();
        for (run, line) in [(1, "Never delete a fixture."), (2, "Keep commits small.")] {
            let p = RuleProposal {
                run,
                line: line.into(),
                reason: "it happened".into(),
            };
            store.insert_proposal(&p, 1).unwrap();
        }
        let api = Api::start_with_store(store, Config::default()).await;
        let rules = api.home().join("rules.md");
        std::fs::write(&rules, "# rules\n- Be careful.\n").unwrap();

        let open: Vec<Proposal> = api
            .get_as("/v1/proposals", "localhost")
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(open.len(), 2);

        let res = api.post("/v1/proposals/1/approve", &()).await;
        assert_eq!(res.status(), StatusCode::OK);
        let after = std::fs::read_to_string(&rules).unwrap();
        assert_eq!(after, "# rules\n- Be careful.\n- Never delete a fixture.\n");
        let again = api.post("/v1/proposals/1/approve", &()).await;
        assert_eq!(again.status(), StatusCode::CONFLICT);
        assert_eq!(std::fs::read_to_string(&rules).unwrap(), after);

        let res = api.post("/v1/proposals/2/reject", &()).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(std::fs::read_to_string(&rules).unwrap(), after);
        let unknown = api.post("/v1/proposals/9/reject", &()).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        let all: Vec<Proposal> = api
            .get_as("/v1/proposals", "localhost")
            .await
            .json()
            .await
            .unwrap();
        assert!(all.iter().all(|p| p.status != ProposalStatus::Open));
    }

    #[tokio::test]
    async fn the_list_shows_open_first_and_a_full_rules_file_refuses_the_line() {
        let mut store = Store::open_in_memory().unwrap();
        for run in [1, 2] {
            let p = RuleProposal {
                run,
                line: format!("Rule {run}."),
                reason: "it happened".into(),
            };
            store.insert_proposal(&p, 1).unwrap();
        }
        store
            .decide_proposal(2, ProposalStatus::Rejected, 2)
            .unwrap();
        let api = Api::start_with_store(store, Config::default()).await;
        let all: Vec<Proposal> = api
            .get_as("/v1/proposals", "localhost")
            .await
            .json()
            .await
            .unwrap();
        // Proposal 2 is newer but decided: the open one comes first.
        assert_eq!(all.iter().map(|p| p.id).collect::<Vec<_>>(), [1, 2]);

        let rules = api.home().join("rules.md");
        let full = format!("{}\n", "x".repeat(MAX_RULES_BYTES - 1));
        std::fs::write(&rules, &full).unwrap();
        let res = api.post("/v1/proposals/1/approve", &()).await;
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert!(res.text().await.unwrap().contains("16 KiB limit"));
        assert_eq!(std::fs::read_to_string(&rules).unwrap(), full);
    }
}
