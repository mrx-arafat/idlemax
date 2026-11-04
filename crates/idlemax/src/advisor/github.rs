//! Adapter: reads open PRs and issues for the logged-in `gh` user (read-only searches)
//! and translates them into `GhItem`s.

use std::time::Duration;

use idlemax_core::advice::{GhItem, GithubSnapshot, GithubStatus};
use serde::Deserialize;
use tokio::process::Command;

const PR_FIELDS: &str = "number,title,url,repository,author,isDraft,createdAt,updatedAt";
const ISSUE_FIELDS: &str = "number,title,url,repository,author,createdAt,updatedAt";
const LIMIT: &str = "50";

#[derive(Debug, thiserror::Error)]
pub enum GhError {
    #[error("gh is not installed")]
    Missing,
    #[error("running gh: {0}")]
    Spawn(std::io::Error),
    #[error("gh timed out")]
    Timeout,
    #[error("gh exited with {code:?}: {stderr}")]
    Exit { code: Option<i32>, stderr: String },
    #[error("parsing gh output: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("bad timestamp {0:?}")]
    Time(String),
}

/// Review requests, assigned PRs and assigned issues, each search bounded by `timeout`.
pub async fn read(timeout: Duration) -> GithubSnapshot {
    let (review, prs, issues) = tokio::join!(
        search(
            &["prs", "--review-requested=@me", "--json", PR_FIELDS],
            timeout
        ),
        search(&["prs", "--assignee=@me", "--json", PR_FIELDS], timeout),
        search(
            &["issues", "--assignee=@me", "--json", ISSUE_FIELDS],
            timeout
        ),
    );
    let results = [&review, &prs, &issues];
    let status = if results.iter().any(|r| matches!(r, Err(GhError::Missing))) {
        GithubStatus::Missing
    } else if let Some(Err(e)) = results.iter().find(|r| r.is_err()) {
        GithubStatus::Failed(e.to_string())
    } else {
        GithubStatus::Ok
    };
    GithubSnapshot {
        status,
        review_requested: review.unwrap_or_default(),
        assigned_prs: prs.unwrap_or_default(),
        assigned_issues: issues.unwrap_or_default(),
    }
}

async fn search(args: &[&str], timeout: Duration) -> Result<Vec<GhItem>, GhError> {
    let mut cmd = Command::new("gh");
    cmd.arg("search")
        .args(args)
        .args(["--state=open", "--limit", LIMIT])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => return Err(GhError::Timeout),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err(GhError::Missing),
        Ok(Err(e)) => return Err(GhError::Spawn(e)),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(300)
            .collect();
        return Err(GhError::Exit {
            code: out.status.code(),
            stderr,
        });
    }
    parse_items(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawItem {
    number: u64,
    title: String,
    url: String,
    repository: RawRepo,
    author: Option<RawAuthor>,
    #[serde(default)]
    is_draft: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct RawAuthor {
    login: String,
}

/// Parses `gh search ... --json` output.
pub fn parse_items(json: &str) -> Result<Vec<GhItem>, GhError> {
    let raw: Vec<RawItem> = serde_json::from_str(json)?;
    raw.into_iter()
        .map(|r| {
            Ok(GhItem {
                repo: r.repository.name_with_owner,
                number: r.number,
                title: r.title,
                url: r.url,
                author: r.author.map(|a| a.login).unwrap_or_default(),
                is_draft: r.is_draft,
                created_ms: millis(&r.created_at)?,
                updated_ms: millis(&r.updated_at)?,
            })
        })
        .collect()
}

fn millis(ts: &str) -> Result<i64, GhError> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| t.timestamp_millis())
        .map_err(|_| GhError::Time(ts.to_string()))
}
