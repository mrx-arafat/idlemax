//! Domain: the dispatcher decides which AI workers would run where, and why. Pure.
//!
//! The rules here decide whether and how many (D2); the orchestrator only chooses among what
//! they offer.

use serde::{Deserialize, Serialize};

pub mod accounts;
pub mod check;
pub mod inputs;
pub mod label;
pub mod orchestrator;
pub mod plan;
pub mod project;
pub mod prompt;
pub mod roadmap;
pub mod room;
pub mod task;
pub mod view;

/// What the dispatcher does with its plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Plans nothing and queues no dispatcher job.
    Off,
    /// Shows what it would launch and launches nothing.
    #[default]
    Propose,
    /// Launches the planned workers itself, one at a time, inside the fence (D3).
    Auto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Propose => "propose",
            Mode::Auto => "auto",
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

/// FNV-1a 64-bit hash of the parts joined by a zero byte, as 16 hex digits.
pub fn fnv64(parts: &[&str]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for (i, part) in parts.iter().enumerate() {
        let sep: &[u8] = if i == 0 { &[] } else { &[0] };
        for byte in sep.iter().chain(part.as_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn fnv64_matches_the_reference_and_separates_parts() {
        assert_eq!(super::fnv64(&["a"]), "af63dc4c8601ec8c");
        assert_ne!(super::fnv64(&["ab", "c"]), super::fnv64(&["a", "bc"]));
    }
}
