//! Domain: what an agent CLI is started with and which environment a worker pane may see:
//! the lean, fenced arguments of each CLI (D10 item 9, D22), the pane's environment pairs and
//! the check of the environment the agent really got. Pure.

use super::WorkerCli;
use crate::job_run::{JOB_ID_ENV, JOB_TAG_ENV};
use crate::types::JobId;
use crate::TOKEN_ENV;

/// Variables a pane always gets empty, besides the names a first probe found (D22, A14).
const ALWAYS_BLANK: [&str; 4] = [TOKEN_ENV, "GH_TOKEN", "GITHUB_TOKEN", "SSH_AUTH_SOCK"];
/// Name endings that mark a credential.
const SECRET_SUFFIXES: [&str; 5] = ["_TOKEN", "_KEY", "_SECRET", "_PASSWORD", "_CREDENTIALS"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    #[error("{cli} workers are not supported yet: {missing}")]
    Unsupported {
        cli: &'static str,
        missing: &'static str,
    },
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

/// The server names of `codex mcp list`: the first column, without the header.
pub fn mcp_server_names(list: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.eq_ignore_ascii_case("name"))
        .filter(|name| {
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .map(str::to_string)
        .collect()
}

/// The `--env` pairs of a worker pane: the job id (the worker marker, A12) and the attempt
/// tag, no git prompt, git settings that give every remote a dead push address and no
/// credential helper, and empty values for the idlemax token and every credential name in
/// `blank` (D6, D22, A14).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn claude_starts_lean_with_its_session_id_and_resumes_it() {
        let first = agent_args(WorkerCli::Claude, "sonnet", Session::New("u-1"), "/g", &[]);
        assert_eq!(
            first.unwrap(),
            strings(&[
                "--model",
                "sonnet",
                "--strict-mcp-config",
                "--permission-mode",
                "bypassPermissions",
                "--session-id",
                "u-1"
            ])
        );
        let again = agent_args(
            WorkerCli::Claude,
            "sonnet",
            Session::Resume("u-1"),
            "/g",
            &[],
        );
        assert_eq!(again.unwrap()[5..], strings(&["--resume", "u-1"]));
    }

    #[test]
    fn codex_runs_in_its_sandbox_with_each_mcp_server_switched_off() {
        let servers = strings(&["github", "notes"]);
        let args = agent_args(
            WorkerCli::Codex,
            "gpt",
            Session::New("x"),
            "/r/.git",
            &servers,
        );
        let want = strings(&[
            "--model",
            "gpt",
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
            "--add-dir",
            "/r/.git",
            "-c",
            "mcp_servers.github.enabled=false",
            "-c",
            "mcp_servers.notes.enabled=false",
        ]);
        assert_eq!(args.unwrap(), want);
        let resumed = agent_args(
            WorkerCli::Codex,
            "gpt",
            Session::Resume("s9"),
            "/r/.git",
            &[],
        );
        assert_eq!(resumed.unwrap()[..2], strings(&["resume", "s9"]));
    }

    #[test]
    fn opencode_and_agy_are_refused_naming_what_is_missing() {
        for cli in [WorkerCli::Opencode, WorkerCli::Agy] {
            let err = agent_args(cli, "m", Session::New("x"), "/g", &[]).unwrap_err();
            assert!(err.to_string().contains("MCP servers"), "{err}");
            assert!(err.to_string().starts_with(cli.as_str()), "{err}");
        }
    }

    #[test]
    fn codex_mcp_list_gives_the_first_column() {
        let list = "Name    Command  Args\ngithub  npx      -y gh\n\nnotes   node     n.js\n";
        assert_eq!(mcp_server_names(list), strings(&["github", "notes"]));
    }

    #[test]
    fn env_problems_names_credentials_and_nothing_else() {
        let vars = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        let bad = vars(&[
            ("IDLEMAX_TOKEN", "t"),
            ("GH_TOKEN", "g"),
            ("SSH_AUTH_SOCK", "/s"),
            ("ANTHROPIC_API_KEY", "k"),
            ("DATABASE_URL", "pg://"),
        ]);
        assert_eq!(
            env_problems(&bad),
            strings(&[
                "IDLEMAX_TOKEN",
                "GH_TOKEN",
                "SSH_AUTH_SOCK",
                "ANTHROPIC_API_KEY",
                "DATABASE_URL"
            ])
        );
        let fine = vars(&[
            ("PATH", "/bin"),
            ("HOME", "/h"),
            ("TERM", "xterm"),
            ("HERDR_PANE_ID", "p1"),
            ("IDLEMAX_JOB_ID", "7"),
            ("GH_TOKEN", ""),
        ]);
        assert!(env_problems(&fine).is_empty());
    }

    #[test]
    fn pane_env_blanks_the_given_names_and_carries_the_job_id() {
        let env = pane_env(7, 99, &strings(&["origin"]), &strings(&["FOO_TOKEN"]));
        let has = |k: &str, v: &str| env.iter().any(|(a, b)| a == k && b == v);
        assert!(has("FOO_TOKEN", ""));
        assert!(has("IDLEMAX_JOB_ID", "7"));
        assert!(has("IDLEMAX_JOB_TAG", "99"));
        assert!(has("IDLEMAX_TOKEN", ""));
        assert!(has("GIT_CONFIG_COUNT", "2"));
        assert!(has("GIT_CONFIG_KEY_1", "remote.origin.pushurl"));
        assert!(has("GIT_CONFIG_VALUE_1", "idlemax-no-push://origin"));
        assert!(env_problems(&env).is_empty());
    }
}
