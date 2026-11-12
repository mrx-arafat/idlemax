//! Domain: the hard limits the owner may change through the coordinator API (D18.1), and the
//! wire shapes of that route. Only keys on `OWNER_LIMITS` can be changed this way, so the token
//! alone never reaches the allowlist or a `coordinator.*` key from a browser. Pure.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::edit::{set_setting, unset_setting, Edit, EditError};
use super::settings::{list_settings, Setting};
use super::{ConfigError, ReloadReport};
use crate::types::Os;

/// The limits, as exact `section.name` keys: the lines of D14, the review cap and the owner's
/// quota reserve. The mode is not one of them: it has its own route (A1).
pub const OWNER_LIMITS: &[&str] = &[
    "dispatcher.hold_mem_used_pct",
    "dispatcher.shed_mem_used_pct",
    "dispatcher.emergency_mem_used_pct",
    "dispatcher.hold_disk_free_mib",
    "dispatcher.shed_disk_free_mib",
    "dispatcher.emergency_disk_free_mib",
    "dispatcher.swap_in_low_pages_per_sec",
    "dispatcher.swap_in_high_pages_per_sec",
    "workers.review_cap",
    "quota.owner_reserve_pct",
];

#[derive(Debug, thiserror::Error)]
pub enum LimitError {
    #[error(
        "`{0}` is not a limit that can be changed here; change it with `idlemax config set` on \
         the device"
    )]
    NotALimit(String),
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// `POST /v1/limits`: `value` null puts the limit back to its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRequest {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// What one change did; `report` says which daemon applies it now and which needs a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitReply {
    pub key: String,
    pub before: Value,
    pub after: Value,
    pub changed: bool,
    pub report: ReloadReport,
}

/// The limits in effect for the `config.toml` text, in the order of `OWNER_LIMITS`.
pub fn limit_settings(text: &str, os: Os) -> Result<Vec<Setting>, ConfigError> {
    let all = list_settings(text, os)?;
    Ok(OWNER_LIMITS
        .iter()
        .filter_map(|key| all.iter().find(|s| s.key == *key).cloned())
        .collect())
}

/// Sets the limit `key` to `raw`, or removes it from the file when `raw` is `None`. Any key off
/// the list is `NotALimit`; a value that would stop a start is the editor's `Invalid`.
pub fn set_limit(text: &str, key: &str, raw: Option<&str>, os: Os) -> Result<Edit, LimitError> {
    if !OWNER_LIMITS.contains(&key) {
        return Err(LimitError::NotALimit(key.to_string()));
    }
    let edit = match raw {
        Some(raw) => set_setting(text, key, raw, os)?,
        None => unset_setting(text, key, os)?,
    };
    Ok(edit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OSES: [Os; 2] = [Os::Macos, Os::Linux];

    #[test]
    fn the_limits_are_real_settings_and_none_is_an_agent_or_server_key() {
        for os in OSES {
            let all = list_settings("", os).unwrap();
            for key in OWNER_LIMITS {
                assert!(all.iter().any(|s| s.key == *key), "{key} on {os:?}");
                for forbidden in ["agent.", "coordinator.", "advisor."] {
                    assert!(!key.starts_with(forbidden), "{key}");
                }
            }
            let shown: Vec<String> = limit_settings("", os)
                .unwrap()
                .into_iter()
                .map(|s| s.key)
                .collect();
            assert_eq!(shown, OWNER_LIMITS);
        }
    }

    #[test]
    fn only_listed_keys_change_and_a_bad_value_gets_the_editors_reason() {
        for key in ["agent.command_allowlist", "dispatcher.mode"] {
            let refused = set_limit("", key, Some("x"), Os::Linux).unwrap_err();
            assert!(matches!(&refused, LimitError::NotALimit(k) if k == key));
            let refused = set_limit("", key, None, Os::Linux).unwrap_err();
            assert!(matches!(refused, LimitError::NotALimit(_)));
        }
        let bad = set_limit("", "dispatcher.hold_mem_used_pct", Some("97"), Os::Linux);
        let Err(LimitError::Edit(EditError::Invalid { reason, .. })) = bad else {
            panic!("{bad:?}");
        };
        assert!(
            reason.contains("must be below shed_mem_used_pct"),
            "{reason}"
        );

        for (key, raw) in [
            ("workers.review_cap", "2"),
            ("quota.owner_reserve_pct", "40"),
        ] {
            let set = set_limit("", key, Some(raw), Os::Linux).unwrap();
            assert!(set.changed, "{key}");
        }
        let set = set_limit("", "dispatcher.hold_mem_used_pct", Some("85"), Os::Linux).unwrap();
        assert!(set.changed && set.text.contains("hold_mem_used_pct = 85"));
        let unset = set_limit(&set.text, "dispatcher.hold_mem_used_pct", None, Os::Linux).unwrap();
        assert!(unset.changed && !unset.text.contains("hold_mem_used_pct"));
    }
}
