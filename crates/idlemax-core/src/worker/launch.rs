//! Domain: what an agent CLI is started with and which environment a worker pane may see:
//! the lean, fenced arguments of each CLI (D10 item 9, D22), the pane's environment pairs and
//! the check of the environment the agent really got. Pure.

use super::checks::REVIEW_SCHEMA;
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
    fn a_reviewer_runs_quiet_and_read_only() {
        let claude = reviewer_args(WorkerCli::Claude, "opus", "/s.json", "/o.json").unwrap();
        assert_eq!(claude[..3], strings(&["claude", "-p", "--settings"]));
        assert!(claude.contains(&"--strict-mcp-config".to_string()));
        assert!(claude.contains(&REVIEW_SCHEMA.to_string()));
        assert_eq!(claude[claude.len() - 2..], strings(&["--model", "opus"]));
        let codex = reviewer_args(WorkerCli::Codex, "gpt", "/s.json", "/o.json").unwrap();
        assert_eq!(
            codex,
            strings(&[
                "codex",
                "exec",
                "--sandbox",
                "read-only",
                "--ephemeral",
                "--ignore-user-config",
                "--output-schema",
                "/s.json",
                "-o",
                "/o.json",
                "-m",
                "gpt"
            ])
        );
        assert!(reviewer_args(WorkerCli::Agy, "m", "/s", "/o").is_err());
    }

    #[test]
    fn codex_mcp_list_gives_the_first_column_or_fails_closed() {
        let list = "Name    Command  Args\ngithub  npx      -y gh\n\nnotes   node     n.js\n";
        assert_eq!(
            mcp_server_names(list).unwrap(),
            strings(&["github", "notes"])
        );
        assert!(mcp_server_names(&format!("{NO_MCP_SERVERS}\n"))
            .unwrap()
            .is_empty());
        let named_name = "Name  Command\nname  npx\n";
        assert_eq!(mcp_server_names(named_name).unwrap(), strings(&["name"]));
        for bad in [
            "Name  Command\nmy.server  npx\n",
            "No MCP servers here, maybe\n",
            "github  npx\n",
            "",
        ] {
            assert!(mcp_server_names(bad).is_err(), "{bad:?}");
        }
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
