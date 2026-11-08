//! Domain: builds a `JobSpec` from the flags of `idlemax job add -- <argv>`.
//! Pure: the caller passes the current directory, nothing here reads the disk.

use std::path::{Component, Path, PathBuf};

use crate::types::{CostClass, JobSpec, SpecError};

/// The flags of `job add`, before defaults. `None` and empty mean "not given".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobDraft {
    /// The command and its arguments; never run through a shell.
    pub argv: Vec<String>,
    pub name: Option<String>,
    pub cost: Option<CostClass>,
    pub devices: Vec<String>,
    pub requires: Vec<String>,
    pub mem_mb: Option<u64>,
    pub max_runtime_secs: Option<u64>,
    pub priority: Option<i32>,
    pub ai_account: Option<String>,
    /// As typed; made absolute against the current directory.
    pub cwd: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DraftError {
    #[error("no command given; put it after `--`, e.g. `idlemax job add -- cargo test`")]
    NoCommand,
    #[error("--ai-account needs an AI job; drop --cost or use --cost ai")]
    AccountNeedsAiCost,
    #[error("--cost ai needs --ai-account <name> (an account from [[quota.accounts]])")]
    AiCostNeedsAccount,
    #[error("--cwd {0:?} is not valid UTF-8")]
    CwdNotUtf8(String),
    #[error(transparent)]
    Spec(#[from] SpecError),
}

impl JobDraft {
    /// The validated spec. `current_dir` resolves a relative `--cwd`; the path is not
    /// checked, because it must exist on the device that runs the job, not on this one.
    pub fn into_spec(self, current_dir: &Path) -> Result<JobSpec, DraftError> {
        let program = self.argv.first().ok_or(DraftError::NoCommand)?;
        let defaults = JobSpec::default();
        let cost = match (self.cost, &self.ai_account) {
            (None, Some(_)) => CostClass::Ai,
            (Some(CostClass::Ai), None) => return Err(DraftError::AiCostNeedsAccount),
            (Some(c), Some(_)) if c != CostClass::Ai => return Err(DraftError::AccountNeedsAiCost),
            (cost, _) => cost.unwrap_or(defaults.cost),
        };
        let cwd = self
            .cwd
            .map(|typed| absolute(&typed, current_dir))
            .transpose()?;
        let spec = JobSpec {
            name: self.name.unwrap_or_else(|| file_name(program)),
            key: self.key,
            command: self.argv,
            cwd,
            requires: self.requires,
            devices: self.devices,
            priority: self.priority.unwrap_or(defaults.priority),
            cost,
            mem_mb: self.mem_mb.unwrap_or(defaults.mem_mb),
            max_runtime_secs: self.max_runtime_secs.unwrap_or(defaults.max_runtime_secs),
            ai_account: self.ai_account,
            ..defaults
        };
        spec.validate()?;
        Ok(spec)
    }
}

/// `/usr/bin/cargo` is named "cargo"; anything without a file name keeps its text.
fn file_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program)
        .to_string()
}

/// `typed` joined onto `base` with `.` and `..` resolved by text, not by the disk.
fn absolute(typed: &str, base: &Path) -> Result<String, DraftError> {
    let mut out = PathBuf::new();
    for part in base.join(typed).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_str()
        .map(str::to_string)
        .ok_or_else(|| DraftError::CwdNotUtf8(typed.to_string()))
}

#[cfg(test)]
mod tests;
