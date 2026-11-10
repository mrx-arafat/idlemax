//! Adapter: where Claude Code, Codex and OpenCode keep their hook files, and which of them a
//! command acts on.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use idlemax_core::install::opencode;

use super::Which;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Agent {
    Claude,
    Codex,
    OpenCode,
}

impl Agent {
    pub(super) const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::OpenCode];

    pub(super) fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }
}

/// Each agent's config directory, from `HOME` and the variables the agents themselves honour.
pub(super) struct Places {
    pub(super) claude: PathBuf,
    pub(super) codex: PathBuf,
    pub(super) opencode: PathBuf,
    pub(super) opencode_legacy: Vec<PathBuf>,
}

impl Places {
    pub(super) fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let home = var("HOME").map(PathBuf::from);
        let under = |dir: &str| home.as_ref().map(|h| h.join(dir));
        let claude = var("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| under(".claude"));
        let codex = var("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| under(".codex"));
        let global = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| under(".config"))
            .map(|dir| dir.join("opencode"));
        // Current OpenCode's global layer takes the explicit config directory first;
        // older releases also search it after their global directory.
        let opencode = var("OPENCODE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| global.clone());
        match (claude, codex, opencode) {
            (Some(claude), Some(codex), Some(opencode)) => {
                let mut opencode_legacy = Vec::new();
                for dir in [global, under(".config/opencode")].into_iter().flatten() {
                    let file = dir.join("plugins").join(opencode::FILE_NAME);
                    if dir != opencode && !opencode_legacy.contains(&file) { opencode_legacy.push(file); }
                }
                Ok(Self {
                claude,
                codex,
                opencode,
                opencode_legacy,
            })},
            _ => bail!("HOME is unset or empty; set HOME, or CLAUDE_CONFIG_DIR, CODEX_HOME and OPENCODE_CONFIG_DIR (or XDG_CONFIG_HOME)"),
        }
    }

    pub(super) fn dir(&self, agent: Agent) -> &Path {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
            Agent::OpenCode => &self.opencode,
        }
    }

    pub(super) fn found(&self, agent: Agent) -> bool {
        self.dir(agent).is_dir()
            || (agent == Agent::OpenCode
                && self.opencode_legacy.iter().any(|path| {
                    path.parent()
                        .and_then(Path::parent)
                        .is_some_and(Path::is_dir)
                }))
    }

    pub(super) fn opencode_files(&self) -> Vec<PathBuf> {
        std::iter::once(self.file(Agent::OpenCode))
            .chain(self.opencode_legacy.iter().cloned())
            .collect()
    }

    /// The file idlemax edits for this agent.
    pub(super) fn file(&self, agent: Agent) -> PathBuf {
        match agent {
            Agent::Claude => self.claude.join("settings.json"),
            Agent::Codex => self.codex.join("hooks.json"),
            Agent::OpenCode => self.opencode.join("plugins").join(opencode::FILE_NAME),
        }
    }

    pub(super) fn codex_config(&self) -> PathBuf {
        self.codex.join("config.toml")
    }
}

/// The agents to act on: the ones named, else every one whose config directory exists.
/// The flag says whether the user named it, which allows creating its directory.
pub(super) fn selected(which: &Which, places: &Places) -> Vec<(Agent, bool)> {
    let named = [which.claude, which.codex, which.opencode];
    if named.iter().any(|on| *on) {
        return Agent::ALL
            .into_iter()
            .zip(named)
            .filter(|(_, on)| *on)
            .collect();
    }
    Agent::ALL
        .into_iter()
        .map(|a| (a, false))
        .filter(|(a, _)| places.found(*a))
        .collect()
}
