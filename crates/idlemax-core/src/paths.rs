//! Domain: state dir layout and the IDLEMAX_HOME rule. Pure; callers pass env values in.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::types::JobId;

pub const HOME_ENV: &str = "IDLEMAX_HOME";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HomeError {
    #[error("IDLEMAX_HOME is set but empty")]
    Empty,
    #[error("cannot locate the home directory: HOME is unset or empty; set IDLEMAX_HOME")]
    Missing,
}

/// `$IDLEMAX_HOME`, else `<user home>/.idlemax`. Set but empty is an error, and an
/// empty user home never falls back to the passwd entry.
pub fn resolve_home(
    idlemax_home: Option<OsString>,
    user_home: Option<OsString>,
) -> Result<PathBuf, HomeError> {
    match idlemax_home {
        Some(v) if v.is_empty() => Err(HomeError::Empty),
        Some(v) => Ok(PathBuf::from(v)),
        None => match user_home {
            Some(h) if !h.is_empty() => Ok(PathBuf::from(h).join(".idlemax")),
            _ => Err(HomeError::Missing),
        },
    }
}

/// All paths under one state dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn token_file(&self) -> PathBuf {
        self.root.join("token")
    }

    pub fn db_file(&self) -> PathBuf {
        self.root.join("coord.db")
    }

    /// Kill switch: while this file exists the agent pauses everything.
    pub fn pause_file(&self) -> PathBuf {
        self.root.join("PAUSE")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn job_logs_dir(&self) -> PathBuf {
        self.logs_dir().join("jobs")
    }

    pub fn job_log_file(&self, id: JobId) -> PathBuf {
        self.job_logs_dir().join(format!("{id}.log"))
    }

    /// Job spec templates (`*.toml`, `idlemax job add` format) the advisor may suggest.
    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join("inbox")
    }

    /// 24 h metrics ring buffer and other agent-local state.
    pub fn agent_state_dir(&self) -> PathBuf {
        self.root.join("agent")
    }

    /// The dispatcher's base checkouts, one per project: `checks/<project id>`.
    pub fn checks_dir(&self) -> PathBuf {
        self.root.join("checks")
    }

    /// Directories the state adapter creates at startup.
    pub fn dirs(&self) -> [PathBuf; 4] {
        [
            self.job_logs_dir(),
            self.inbox_dir(),
            self.agent_state_dir(),
            self.jobs_dir(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_rules() {
        assert_eq!(
            resolve_home(Some("".into()), Some("/u".into())),
            Err(HomeError::Empty)
        );
        assert_eq!(
            resolve_home(Some("/x".into()), None),
            Ok(PathBuf::from("/x"))
        );
        assert_eq!(
            resolve_home(None, Some("/u".into())),
            Ok(PathBuf::from("/u/.idlemax"))
        );
        assert_eq!(resolve_home(None, Some("".into())), Err(HomeError::Missing));
        assert_eq!(resolve_home(None, None), Err(HomeError::Missing));
    }
}
