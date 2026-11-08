//! Domain: what `idlemax service install|uninstall|status` writes and runs, per platform.
//! Pure: the CLI writes the unit file and runs the commands this module describes.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::quote::{is_plain_text, shell_quote, systemd_env_line, systemd_exec_word, xml_escape};
use super::setup::UnitChange;
use crate::types::Os;

const AGENT_PLIST: &str = include_str!("../../../../packaging/macos/com.idlemax.agent.plist");
const COORD_PLIST: &str = include_str!("../../../../packaging/macos/com.idlemax.coord.plist");
const AGENT_UNIT: &str = include_str!("../../../../packaging/linux/idlemax-agent.user.service");
const COORD_UNIT: &str = include_str!("../../../../packaging/linux/idlemax-coord.user.service");

/// Variables copied from the installing shell, because a service starts with a bare environment.
/// All are paths that the agent resolves the same way the CLI does.
pub const PASSTHROUGH_VARS: [&str; 3] = ["IDLEMAX_HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    Agent,
    Coord,
}

impl ServiceKind {
    pub const ALL: [ServiceKind; 2] = [ServiceKind::Agent, ServiceKind::Coord];

    pub fn name(self) -> &'static str {
        match self {
            ServiceKind::Agent => "agent",
            ServiceKind::Coord => "coord",
        }
    }

    fn label(self) -> String {
        format!("com.idlemax.{}", self.name())
    }

    fn unit(self) -> String {
        format!("idlemax-{}.service", self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS LaunchAgent in `~/Library/LaunchAgents`.
    Launchd,
    /// systemd user unit in `~/.config/systemd/user`.
    SystemdUser,
}

impl Platform {
    pub fn for_os(os: Os) -> Option<Platform> {
        match os {
            Os::Macos => Some(Platform::Launchd),
            Os::Linux => Some(Platform::SystemdUser),
            Os::Windows | Os::Other => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error("the binary path {} is not absolute; pass --bin with a full path", .0.display())]
    RelativeBin(PathBuf),
    #[error("{what} is not valid UTF-8 or contains a control character, so it cannot go in a service file")]
    BadValue { what: String },
}

/// Everything the service files are built from, collected once by the CLI.
#[derive(Debug, Clone)]
pub struct ServiceEnv {
    pub bin: PathBuf,
    /// The user's home directory; service files live under it.
    pub home: PathBuf,
    pub state_dir: PathBuf,
    /// `PATH` at install time; the agent needs it to find the CLIs its jobs run.
    pub path: Option<String>,
    /// Non-empty `PASSTHROUGH_VARS` of the installing shell.
    pub vars: Vec<(String, String)>,
    /// Numeric user id, for the launchd `gui/<uid>` domain.
    pub uid: u32,
}

/// One external command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub argv: Vec<String>,
    /// Failure is fine, e.g. unloading a service that is not loaded.
    pub ignore_failure: bool,
}

impl Step {
    fn new(argv: &[&str], ignore_failure: bool) -> Self {
        Step {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            ignore_failure,
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words: Vec<String> = self.argv.iter().map(|a| shell_quote(a)).collect();
        write!(f, "{}", words.join(" "))?;
        if self.ignore_failure {
            write!(f, "  # a failure here is fine")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnitFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    /// Run before deleting the unit file.
    pub before: Vec<Step>,
    /// Run after deleting it.
    pub after: Vec<Step>,
}

pub fn unit_path(platform: Platform, kind: ServiceKind, home: &Path) -> PathBuf {
    match platform {
        Platform::Launchd => home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", kind.label())),
        Platform::SystemdUser => home.join(".config/systemd/user").join(kind.unit()),
    }
}

/// A warning when the binary sits in a cargo build directory that a rebuild or `cargo clean` replaces.
pub fn bin_warning(bin: &Path) -> Option<&'static str> {
    let parts: Vec<_> = bin.components().map(|c| c.as_os_str()).collect();
    let in_target = parts
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"));
    in_target.then_some(
        "the binary is inside a cargo target directory; a rebuild or `cargo clean` would break the service. Install it first (`cargo install --path crates/idlemax`) or pass --bin",
    )
}

fn text<'a>(what: &str, p: &'a Path) -> Result<&'a str, ServiceError> {
    p.to_str()
        .filter(|s| is_plain_text(s))
        .ok_or_else(|| ServiceError::BadValue { what: what.into() })
}

/// The environment rows for this unit as (name, value) pairs.
fn env_rows(kind: ServiceKind, env: &ServiceEnv) -> Vec<(&str, &str)> {
    let mut rows = Vec::new();
    if let (ServiceKind::Agent, Some(path)) = (kind, &env.path) {
        rows.push(("PATH", path.as_str()));
    }
    for (name, value) in &env.vars {
        if kind == ServiceKind::Agent || name == "IDLEMAX_HOME" {
            rows.push((name.as_str(), value.as_str()));
        }
    }
    rows
}

fn plist_env_block(rows: &[(&str, &str)]) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        "\t<key>EnvironmentVariables</key>".into(),
        "\t<dict>".into(),
    ];
    for (name, value) in rows {
        lines.push(format!("\t\t<key>{}</key>", xml_escape(name)));
        lines.push(format!("\t\t<string>{}</string>", xml_escape(value)));
    }
    lines.push("\t</dict>".into());
    lines
}

/// Replaces `@TOKEN@`s in `template`; the `@ENVIRONMENT@` line becomes `env_lines`.
fn expand(template: &str, tokens: &[(&str, String)], env_lines: &[String]) -> String {
    let mut out = Vec::new();
    let mut skip_blank = false;
    for line in template.lines() {
        if skip_blank && line.trim().is_empty() {
            skip_blank = false;
            continue;
        }
        skip_blank = false;
        if line.trim() == "@ENVIRONMENT@" {
            skip_blank = env_lines.is_empty();
            out.extend(env_lines.iter().cloned());
            continue;
        }
        out.push(
            tokens
                .iter()
                .fold(line.to_string(), |acc, (k, v)| acc.replace(k, v)),
        );
    }
    out.join("\n") + "\n"
}

/// The unit file for `kind`, filled in from `env`.
pub fn render(
    platform: Platform,
    kind: ServiceKind,
    env: &ServiceEnv,
) -> Result<UnitFile, ServiceError> {
    if !env.bin.is_absolute() {
        return Err(ServiceError::RelativeBin(env.bin.clone()));
    }
    let bin = text("the binary path", &env.bin)?;
    let state = text("the state dir", &env.state_dir)?;
    text("the home dir", &env.home)?;
    let rows = env_rows(kind, env);
    if rows
        .iter()
        .any(|(n, v)| !is_plain_text(n) || !is_plain_text(v))
    {
        return Err(ServiceError::BadValue {
            what: "an environment value".into(),
        });
    }
    let content = match platform {
        Platform::Launchd => {
            let template = if kind == ServiceKind::Agent {
                AGENT_PLIST
            } else {
                COORD_PLIST
            };
            let tokens = [("@BIN@", xml_escape(bin)), ("@STATE@", xml_escape(state))];
            expand(template, &tokens, &plist_env_block(&rows))
        }
        Platform::SystemdUser => {
            let template = if kind == ServiceKind::Agent {
                AGENT_UNIT
            } else {
                COORD_UNIT
            };
            let lines: Vec<String> = rows.iter().map(|(n, v)| systemd_env_line(n, v)).collect();
            expand(template, &[("@BIN@", systemd_exec_word(bin))], &lines)
        }
    };
    Ok(UnitFile {
        path: unit_path(platform, kind, &env.home),
        content,
    })
}

fn launchd_target(kind: ServiceKind, env: &ServiceEnv) -> String {
    format!("gui/{}/{}", env.uid, kind.label())
}

/// Commands that start the service after its file is written. Safe to repeat. A running
/// systemd unit keeps its old ExecStart, PATH and IDLEMAX_HOME until it restarts, so a
/// changed unit is restarted; an unchanged one is only started if it is stopped.
pub fn install_steps(
    platform: Platform,
    kind: ServiceKind,
    env: &ServiceEnv,
    change: UnitChange,
) -> Vec<Step> {
    match platform {
        Platform::Launchd => {
            let file = unit_path(platform, kind, &env.home);
            let domain = format!("gui/{}", env.uid);
            vec![
                Step::new(&["launchctl", "bootout", &launchd_target(kind, env)], true),
                Step::new(
                    &["launchctl", "bootstrap", &domain, &file.to_string_lossy()],
                    false,
                ),
            ]
        }
        Platform::SystemdUser => {
            let unit = kind.unit();
            let start = if change == UnitChange::Same {
                "start"
            } else {
                "restart"
            };
            let mut steps = Vec::new();
            if change != UnitChange::Same {
                steps.push(Step::new(&["systemctl", "--user", "daemon-reload"], false));
            }
            steps.push(Step::new(&["systemctl", "--user", "enable", &unit], false));
            steps.push(Step::new(&["systemctl", "--user", start, &unit], false));
            steps
        }
    }
}

/// Commands that stop the service around deleting its file. The caller runs `before` only for
/// a loaded service, and a failure of it must keep the file: the service is still running.
pub fn removal(platform: Platform, kind: ServiceKind, env: &ServiceEnv) -> Removal {
    match platform {
        Platform::Launchd => Removal {
            before: vec![Step::new(
                &["launchctl", "bootout", &launchd_target(kind, env)],
                false,
            )],
            after: Vec::new(),
        },
        Platform::SystemdUser => Removal {
            before: vec![Step::new(
                &["systemctl", "--user", "disable", "--now", &kind.unit()],
                false,
            )],
            after: vec![Step::new(&["systemctl", "--user", "daemon-reload"], false)],
        },
    }
}

/// The read-only command whose output `parse_status` understands.
pub fn status_step(platform: Platform, kind: ServiceKind, env: &ServiceEnv) -> Step {
    match platform {
        Platform::Launchd => Step::new(&["launchctl", "print", &launchd_target(kind, env)], false),
        Platform::SystemdUser => Step::new(
            &[
                "systemctl",
                "--user",
                "show",
                &kind.unit(),
                "--property=LoadState,ActiveState",
            ],
            false,
        ),
    }
}

/// What running `status_step` told us.
#[derive(Debug, Clone, Copy)]
pub enum Probe<'a> {
    /// The unit file is absent, so nothing was asked.
    NotInstalled,
    /// The command could not be started.
    Missing(&'a str),
    Ran {
        ok: bool,
        stdout: &'a str,
        stderr: &'a str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    pub kind: ServiceKind,
    pub file: PathBuf,
    pub installed: bool,
    /// `None` when the service manager could not be asked; `note` says why.
    pub loaded: Option<bool>,
    pub running: Option<bool>,
    pub note: Option<String>,
}

pub fn parse_status(
    platform: Platform,
    kind: ServiceKind,
    file: PathBuf,
    probe: Probe,
) -> ServiceStatus {
    let mut s = ServiceStatus {
        kind,
        file,
        installed: true,
        loaded: None,
        running: None,
        note: None,
    };
    match (probe, platform) {
        (Probe::NotInstalled, _) => {
            s.installed = false;
            (s.loaded, s.running) = (Some(false), Some(false));
        }
        (Probe::Missing(tool), _) => s.note = Some(format!("{tool} not found, cannot check")),
        (Probe::Ran { ok, stdout, .. }, Platform::Launchd) => {
            s.loaded = Some(ok);
            s.running = Some(ok && stdout.lines().any(|l| l.trim() == "state = running"));
        }
        (
            Probe::Ran {
                ok: true, stdout, ..
            },
            Platform::SystemdUser,
        ) => {
            let prop = |key: &str| {
                stdout
                    .lines()
                    .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            };
            s.loaded = Some(prop("LoadState") == Some("loaded"));
            s.running = Some(prop("ActiveState") == Some("active"));
        }
        (Probe::Ran { stderr, .. }, Platform::SystemdUser) => {
            let why = stderr.lines().next().unwrap_or("no user session bus");
            s.note = Some(format!("systemctl --user failed: {why}"));
        }
    }
    s
}

#[cfg(test)]
mod tests;
