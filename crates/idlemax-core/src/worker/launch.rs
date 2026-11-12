//! Domain: what an agent CLI is started with and which environment a worker pane may see:
//! the lean, fenced arguments of each CLI (D10 item 9, D22), the pane's environment pairs and
//! the check of the environment the agent really got. Pure.

use super::checks::REVIEW_SCHEMA;
use super::WorkerCli;
use crate::config::WorkerHostChoice;
use crate::job_run::{JOB_ID_ENV, JOB_TAG_ENV};
use crate::types::{DeviceProfile, JobId};
use crate::TOKEN_ENV;

/// Variables a pane always gets empty, besides the names a first probe found (D22, A14).
const ALWAYS_BLANK: [&str; 6] = [
    TOKEN_ENV,
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSH_AUTH_SOCK",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
];
/// URL prefixes git rewrites to the dead push address, so a push by URL or through a remote
/// added later goes nowhere either: `""` matches every URL, the rest name each scheme.
const NO_PUSH_PREFIXES: [&str; 9] = [
    "", "https://", "http://", "ssh://", "git://", "git@", "file://", "/", ".",
];
/// Name endings that mark a credential.
const SECRET_SUFFIXES: [&str; 5] = ["_TOKEN", "_KEY", "_SECRET", "_PASSWORD", "_CREDENTIALS"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("{cli} workers are not supported yet: {missing}")]
    Unsupported {
        cli: &'static str,
        missing: &'static str,
    },
    #[error("codex mcp list could not be read ({0}); no worker starts with a server left on")]
    McpList(String),
}

/// Which session the agent starts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session<'a> {
    /// A first start. Claude takes the id idlemax made, so a stop a second later can still be
    /// continued; Codex makes its own, read back from Herdr.
    New(&'a str),
    /// Continue this session (D11).
    Resume(&'a str),
}

/// The arguments after the program name. `git_common_dir` is the worktree's
/// `git rev-parse --git-common-dir`, where a commit writes; `mcp_servers` are the names Codex
/// has configured, each switched off.
pub fn agent_args(
    cli: WorkerCli,
    model: &str,
    session: Session<'_>,
    git_common_dir: &str,
    mcp_servers: &[String],
) -> Result<Vec<String>, LaunchError> {
    let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match cli {
        WorkerCli::Claude => {
            let mut args = owned(&["--model", model, "--strict-mcp-config"]);
            args.extend(owned(&["--permission-mode", "bypassPermissions"]));
            args.extend(match session {
                Session::New(id) => owned(&["--session-id", id]),
                Session::Resume(id) => owned(&["--resume", id]),
            });
            Ok(args)
        }
        WorkerCli::Codex => {
            let mut args = match session {
                Session::New(_) => Vec::new(),
                Session::Resume(id) => owned(&["resume", id]),
            };
            args.extend(owned(&["--model", model, "--sandbox", "workspace-write"]));
            args.extend(owned(&["--ask-for-approval", "never"]));
            args.extend(owned(&["--add-dir", git_common_dir]));
            for name in mcp_servers {
                args.push("-c".into());
                args.push(format!("mcp_servers.{name}.enabled=false"));
            }
            Ok(args)
        }
        WorkerCli::Opencode => Err(LaunchError::Unsupported {
            cli: "opencode",
            missing: "no proven flag that leaves out the owner's MCP servers",
        }),
        WorkerCli::Agy => Err(LaunchError::Unsupported {
            cli: "agy",
            missing: "no proven flag that leaves out the owner's MCP servers",
        }),
    }
}

/// The arguments of a headless worker start or resume.
/// Claude starts lean with `-p` and its model/permissions flags; Codex reports Unsupported
/// naming the session id (Open Question 4).
pub fn headless_args(
    cli: WorkerCli,
    model: &str,
    session: Session<'_>,
) -> Result<Vec<String>, LaunchError> {
    let owned = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match cli {
        WorkerCli::Claude => {
            let mut args = owned(&["-p", "--model", model, "--strict-mcp-config"]);
            args.extend(owned(&["--permission-mode", "bypassPermissions"]));
            args.extend(match session {
                Session::New(id) => owned(&["--session-id", id]),
                Session::Resume(id) => owned(&["--resume", id]),
            });
            Ok(args)
        }
        WorkerCli::Codex => Err(LaunchError::Unsupported {
            cli: "codex",
            missing: "how `codex exec` reports the session id of a new run is not recorded",
        }),
        WorkerCli::Opencode | WorkerCli::Agy => Err(LaunchError::Unsupported {
            cli: cli.as_str(),
            missing: "only Claude has a proven way to name and continue a print-mode session",
        }),
    }
}

/// Chooses which host runs a worker launch (Spec 5 Task 8): a production device always gets
/// the headless host (A23), because a pane runs outside the agent's cgroup; `headless` and
/// `herdr` are taken as set; `auto` takes Herdr when it answers and the headless host else.
pub fn pick_host(
    setting: WorkerHostChoice,
    profile: Option<DeviceProfile>,
    herdr_answers: bool,
) -> WorkerHostChoice {
    if matches!(profile, Some(DeviceProfile::Production)) {
        return WorkerHostChoice::Headless;
    }
    match setting {
        WorkerHostChoice::Headless => WorkerHostChoice::Headless,
        WorkerHostChoice::Herdr => WorkerHostChoice::Herdr,
        WorkerHostChoice::Auto => {
            if herdr_answers {
                WorkerHostChoice::Herdr
            } else {
                WorkerHostChoice::Headless
            }
        }
    }
}

/// What `codex mcp list` prints when no server is configured.
pub const NO_MCP_SERVERS: &str = "No MCP servers configured yet.";

/// The server names of `codex mcp list`: the first column under a `Name` header. Anything it
/// cannot read for sure is an error, so the launch fails closed instead of leaving a server
/// of the owner switched on in the worker (D10 item 9, D22).
pub fn mcp_server_names(list: &str) -> Result<Vec<String>, LaunchError> {
    let mut lines = list.lines().map(str::trim).filter(|l| !l.is_empty());
    let unreadable = |why: String| LaunchError::McpList(why);
    let Some(header) = lines.next() else {
        return Err(unreadable("it printed nothing".into()));
    };
    if header == NO_MCP_SERVERS {
        return Ok(Vec::new());
    }
    if header.split_whitespace().next() != Some("Name") {
        return Err(unreadable(format!("no Name header in {header:?}")));
    }
    lines
        .map(|line| {
            let name = line.split_whitespace().next().unwrap_or_default();
            let valid = name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            match valid {
                true => Ok(name.to_string()),
                false => Err(unreadable(format!(
                    "server name {name:?} cannot be switched off"
                ))),
            }
        })
        .collect()
}

/// The `--env` pairs of a worker pane: the job id (the worker marker, A12) and the attempt
/// tag, no git prompt, git settings that give every remote and every push URL a dead push
/// address and no credential helper, and empty values for the idlemax token, the askpass
/// helpers and every credential name in `blank` (D6, D22, A14). Only an OS sandbox stops a
/// worker that drops this environment (A13).
pub fn pane_env(
    job_id: JobId,
    tag: u64,
    remotes: &[String],
    blank: &[String],
) -> Vec<(String, String)> {
    let mut env = vec![
        (JOB_ID_ENV.to_string(), job_id.to_string()),
        (JOB_TAG_ENV.to_string(), tag.to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
    ];
    let mut config = vec![("credential.helper".to_string(), String::new())];
    for remote in remotes {
        config.push((
            format!("remote.{remote}.pushurl"),
            format!("idlemax-no-push://{remote}"),
        ));
    }
    for prefix in NO_PUSH_PREFIXES {
        let key = "url.idlemax-no-push://.pushInsteadOf".to_string();
        config.push((key, prefix.to_string()));
    }
    env.push(("GIT_CONFIG_COUNT".to_string(), config.len().to_string()));
    for (i, (key, value)) in config.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), value));
    }
    let names = ALWAYS_BLANK.iter().map(|n| n.to_string());
    for name in names.chain(blank.iter().cloned()) {
        if !env.iter().any(|(k, _)| *k == name) {
            env.push((name, String::new()));
        }
    }
    env
}

/// The names of every credential-like variable with a value; names only, never values.
pub fn env_problems(vars: &[(String, String)]) -> Vec<String> {
    vars.iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, _)| name)
        .filter(|name| is_credential(name))
        .cloned()
        .collect()
}

fn is_credential(name: &str) -> bool {
    name == TOKEN_ENV
        || name == "SSH_AUTH_SOCK"
        || name.starts_with("AWS_")
        || SECRET_SUFFIXES.iter().any(|s| name.ends_with(s))
        || name.contains("DATABASE_URL")
}

/// The argv of an independent review (D8), the program first: Claude with the advisor's quiet
/// flags, or Codex read-only and without the owner's config. The diff and the task go on
/// standard input; Codex writes its answer to `out_file`.
pub fn reviewer_args(
    cli: WorkerCli,
    model: &str,
    schema_file: &str,
    out_file: &str,
) -> Result<Vec<String>, LaunchError> {
    match cli {
        WorkerCli::Claude => {
            let mut argv = vec!["claude".to_string()];
            argv.extend(crate::advice::ai::quiet_claude_args(REVIEW_SCHEMA));
            argv.extend(["--model".to_string(), model.to_string()]);
            Ok(argv)
        }
        WorkerCli::Codex => Ok([
            "codex",
            "exec",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--ignore-user-config",
            "--output-schema",
            schema_file,
            "-o",
            out_file,
            "-m",
            model,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()),
        WorkerCli::Opencode | WorkerCli::Agy => Err(LaunchError::Unsupported {
            cli: cli.as_str(),
            missing: "no proven read-only review mode",
        }),
    }
}

#[cfg(test)]
mod tests;
