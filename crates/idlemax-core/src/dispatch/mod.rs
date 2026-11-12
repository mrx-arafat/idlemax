//! Domain: the dispatcher decides which AI workers would run where, and why. Pure.
//!
//! The rules here decide whether and how many (D2); the orchestrator only chooses among what
//! they offer.

use serde::{Deserialize, Serialize};

pub mod inputs;
pub mod project;
pub mod roadmap;
pub mod room;

/// What the dispatcher does with its plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Plans nothing and queues no dispatcher job.
    Off,
    /// Shows what it would launch and launches nothing.
    #[default]
    Propose,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Propose => "propose",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `POST /v1/dispatcher/mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeRequest {
    pub mode: Mode,
}

/// The reply: the mode now, the mode before, and whether the file changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeReply {
    pub mode: Mode,
    pub before: Mode,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DispatchError {
    #[error("{0}")]
    Invalid(String),
}
