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
        match (claude, codex, under(".config/opencode")) {
            (Some(claude), Some(codex), Some(opencode)) => Ok(Self {
                claude,
                codex,
                opencode,
            }),
            _ => bail!("HOME is unset or empty; set it, or CLAUDE_CONFIG_DIR and CODEX_HOME"),
        }
    }

    pub(super) fn dir(&self, agent: Agent) -> &Path {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
            Agent::OpenCode => &self.opencode,
        }
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
        .filter(|(a, _)| places.dir(*a).is_dir())
        .collect()
}
