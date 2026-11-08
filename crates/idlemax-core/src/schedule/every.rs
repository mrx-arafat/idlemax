//! Domain: the repeat interval of a schedule ("30m", "6h", "1d") and its text form.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

const MIN_SECS: u64 = 60;
const MAX_SECS: u64 = 365 * 86_400;
/// Longest unit first, so a display picks the largest one that divides the interval evenly.
const UNITS: [(char, u64); 4] = [('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EveryError {
    #[error("invalid interval {0:?}: write a whole number and one unit (s, m, h or d), for example 90m, 6h or 1d")]
    Syntax(String),
    #[error("interval {0} is too short: the minimum is 1m")]
    TooShort(String),
    #[error("interval {0} is too long: the maximum is 365d")]
    TooLong(String),
}

/// How often a schedule repeats. JSON and TOML form: the string "1d", "6h", "90m".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Every(u64);

impl Every {
    pub fn secs(self) -> u64 {
        self.0
    }

    /// The interval as a `UnixMs` span.
    pub fn ms(self) -> i64 {
        i64::try_from(self.0.saturating_mul(1000)).unwrap_or(i64::MAX)
    }

    /// Builds an interval from seconds, enforcing the 1m to 365d range.
    pub fn from_secs(secs: u64) -> Result<Self, EveryError> {
        if secs < MIN_SECS {
            Err(EveryError::TooShort(format!("{secs}s")))
        } else if secs > MAX_SECS {
            Err(EveryError::TooLong(format!("{secs}s")))
        } else {
            Ok(Every(secs))
        }
    }
}

impl FromStr for Every {
    type Err = EveryError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let syntax = || EveryError::Syntax(text.to_string());
        let unit = text.chars().last().ok_or_else(syntax)?;
        let per_unit = UNITS
            .iter()
            .find_map(|&(u, secs)| (u == unit).then_some(secs))
            .ok_or_else(syntax)?;
        let digits = &text[..text.len() - unit.len_utf8()];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(syntax());
        }
        let count: u64 = digits
            .parse()
            .map_err(|_| EveryError::TooLong(text.to_string()))?;
        let secs = count
            .checked_mul(per_unit)
            .ok_or_else(|| EveryError::TooLong(text.to_string()))?;
        Every::from_secs(secs).map_err(|e| match e {
            EveryError::TooShort(_) => EveryError::TooShort(text.to_string()),
            EveryError::TooLong(_) => EveryError::TooLong(text.to_string()),
            other => other,
        })
    }
}

impl fmt::Display for Every {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (unit, per_unit) = UNITS
            .iter()
            .copied()
            .find(|&(_, secs)| self.0.is_multiple_of(secs))
            .unwrap_or(('s', 1));
        write!(f, "{}{unit}", self.0 / per_unit)
    }
}

impl From<Every> for String {
    fn from(e: Every) -> String {
        e.to_string()
    }
}

impl TryFrom<String> for Every {
    type Error = EveryError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}
