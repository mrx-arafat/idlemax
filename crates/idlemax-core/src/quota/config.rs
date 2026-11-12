//! Domain: quota configuration, defaults and validation. Pure.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::dispatch::task::TaskSource;
use crate::quota::{AccountConfig, Provider};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaConfig {
    pub accounts: Vec<AccountConfig>,
    pub owner_reserve_pct: f64,
    pub safety_margin_pct: f64,
    pub five_hour_stop_pct: f64,
    pub last_day_floor_pct: f64,
    pub seed_small_pct: f64,
    pub seed_standard_pct: f64,
    pub server_only_margin_pct: f64,
    pub small_task_types: Vec<String>,
    pub claude_small_model: Option<String>,
    pub claude_standard_model: Option<String>,
    pub codex_small_model: Option<String>,
    pub codex_standard_model: Option<String>,
    pub agy_small_model: Option<String>,
    pub agy_standard_model: Option<String>,
    pub opencode_small_model: Option<String>,
    pub opencode_standard_model: Option<String>,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            owner_reserve_pct: 50.0,
            safety_margin_pct: 10.0,
            five_hour_stop_pct: 60.0,
            last_day_floor_pct: 15.0,
            seed_small_pct: 3.0,
            seed_standard_pct: 8.0,
            server_only_margin_pct: 5.0,
            small_task_types: vec!["broken_check".into()],
            claude_small_model: Some("haiku".into()),
            claude_standard_model: Some("sonnet".into()),
            codex_small_model: None,
            codex_standard_model: None,
            agy_small_model: None,
            agy_standard_model: None,
            opencode_small_model: None,
            opencode_standard_model: None,
        }
    }
}

impl QuotaConfig {
    pub fn account(&self, name: &str) -> Option<&AccountConfig> {
        self.accounts.iter().find(|a| a.name == name)
    }

    pub fn validate(&self) -> Result<(), QuotaConfigError> {
        let mut seen = BTreeSet::new();
        for account in &self.accounts {
            validate_account(account, &mut seen)?;
        }
        self.validate_percents()?;
        for entry in &self.small_task_types {
            if entry.parse::<TaskSource>().is_err() {
                return Err(QuotaConfigError::InvalidTaskSource(entry.clone()));
            }
        }
        self.validate_models()?;
        Ok(())
    }

    fn validate_percents(&self) -> Result<(), QuotaConfigError> {
        let pcts = [
            ("quota.owner_reserve_pct", self.owner_reserve_pct),
            ("quota.safety_margin_pct", self.safety_margin_pct),
            ("quota.five_hour_stop_pct", self.five_hour_stop_pct),
            ("quota.last_day_floor_pct", self.last_day_floor_pct),
            ("quota.seed_small_pct", self.seed_small_pct),
            ("quota.seed_standard_pct", self.seed_standard_pct),
            ("quota.server_only_margin_pct", self.server_only_margin_pct),
        ];
        for (key, val) in pcts {
            if !(0.0..=100.0).contains(&val) || val.is_nan() {
                return Err(QuotaConfigError::InvalidPercent { key, value: val });
            }
        }
        Ok(())
    }

    fn validate_models(&self) -> Result<(), QuotaConfigError> {
        let all_models = [
            ("quota.claude_small_model", &self.claude_small_model),
            ("quota.claude_standard_model", &self.claude_standard_model),
            ("quota.codex_small_model", &self.codex_small_model),
            ("quota.codex_standard_model", &self.codex_standard_model),
            ("quota.agy_small_model", &self.agy_small_model),
            ("quota.agy_standard_model", &self.agy_standard_model),
            ("quota.opencode_small_model", &self.opencode_small_model),
            (
                "quota.opencode_standard_model",
                &self.opencode_standard_model,
            ),
        ];
        for (key, opt) in all_models {
            if let Some(model) = opt {
                if model.trim().is_empty() {
                    return Err(QuotaConfigError::EmptyModel { key });
                }
            }
        }

        let claude_models = [
            ("quota.claude_small_model", &self.claude_small_model),
            ("quota.claude_standard_model", &self.claude_standard_model),
        ];
        for (key, model_opt) in claude_models {
            if let Some(model) = model_opt {
                if model.to_lowercase().contains("opus") {
                    return Err(QuotaConfigError::OpusNotAllowed {
                        key,
                        model: model.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_account(
    account: &AccountConfig,
    seen: &mut BTreeSet<String>,
) -> Result<(), QuotaConfigError> {
    let name_ok = account
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if account.name.is_empty() || account.name.starts_with('.') || !name_ok {
        return Err(QuotaConfigError::InvalidName(account.name.clone()));
    }
    if !seen.insert(account.name.clone()) {
        return Err(QuotaConfigError::DuplicateAccount(account.name.clone()));
    }
    if let Some(cap) = account.daily_cap_runs {
        if account.provider != Provider::Agy && account.provider != Provider::Opencode {
            return Err(QuotaConfigError::DailyCapNotSupported {
                account: account.name.clone(),
                provider: account.provider,
            });
        }
        if cap == 0 {
            return Err(QuotaConfigError::ZeroDailyCap(account.name.clone()));
        }
    }
    for w in &account.windows {
        w.validate(&account.name)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum QuotaConfigError {
    /// Names become file names (`quota-<name>.json`): letters, digits, '.', '_' or '-'.
    #[error("quota account name {0:?} must be non-empty letters, digits, '.', '_' or '-'")]
    InvalidName(String),
    #[error("quota account {0} is listed twice")]
    DuplicateAccount(String),
    #[error("quota account {account}: {field} must be within 0..=100, got {value}")]
    PercentOutOfRange {
        account: String,
        field: &'static str,
        value: f64,
    },
    #[error("quota account {0}: rolling window minutes must be > 0")]
    ZeroMinutes(String),
    #[error("setting {key} must be within 0..=100, got {value}")]
    InvalidPercent { key: &'static str, value: f64 },
    #[error("quota account {account}: daily_cap_runs is not supported for {provider:?}; claude and codex accounts are paced by their readings")]
    DailyCapNotSupported { account: String, provider: Provider },
    #[error("quota account {0}: daily_cap_runs must be > 0")]
    ZeroDailyCap(String),
    #[error("quota.small_task_types entry {0:?} is invalid; must be one of: broken_check, roadmap, github_issue, session")]
    InvalidTaskSource(String),
    #[error("setting {key} cannot use Opus ({model:?}); Opus orchestrates only")]
    OpusNotAllowed { key: &'static str, model: String },
    #[error("setting {key} cannot be empty")]
    EmptyModel { key: &'static str },
}

#[cfg(test)]
mod tests {
    use crate::Config;

    #[test]
    fn accepts_opencode_with_daily_cap_and_refuses_claude_daily_cap() {
        let valid = r#"
[[quota.accounts]]
name = "my-opencode"
provider = "opencode"
daily_cap_runs = 5
"#;
        let cfg = Config::parse(valid).expect("opencode with daily_cap_runs should parse");
        assert_eq!(cfg.quota.accounts[0].daily_cap_runs, Some(5));

        let valid_agy = r#"
[[quota.accounts]]
name = "my-agy"
provider = "agy"
daily_cap_runs = 3
"#;
        let cfg_agy = Config::parse(valid_agy).expect("agy with daily_cap_runs should parse");
        assert_eq!(cfg_agy.quota.accounts[0].daily_cap_runs, Some(3));

        let invalid = r#"
[[quota.accounts]]
name = "my-claude"
provider = "claude"
daily_cap_runs = 5
"#;
        let err = Config::parse(invalid).unwrap_err().to_string();
        assert!(err.contains("my-claude"), "expected account name in {err}");
        assert!(
            err.contains("claude and codex accounts are paced by their readings"),
            "expected paced explanation in {err}"
        );
    }

    #[test]
    fn refuses_zero_and_api_daily_cap() {
        let zero_cap = r#"
[[quota.accounts]]
name = "my-opencode"
provider = "opencode"
daily_cap_runs = 0
"#;
        let err = Config::parse(zero_cap).unwrap_err().to_string();
        assert!(
            err.contains("daily_cap_runs must be > 0"),
            "expected zero cap error in {err}"
        );

        let api_cap = r#"
[[quota.accounts]]
name = "my-api"
provider = "api"
daily_cap_runs = 5
"#;
        let err = Config::parse(api_cap).unwrap_err().to_string();
        assert!(
            err.contains("claude and codex accounts are paced by their readings"),
            "expected paced explanation for api cap in {err}"
        );

        let claude_zero = r#"
[[quota.accounts]]
name = "my-claude"
provider = "claude"
daily_cap_runs = 0
"#;
        let err = Config::parse(claude_zero).unwrap_err().to_string();
        assert!(
            err.contains("claude and codex accounts are paced by their readings"),
            "expected paced explanation for claude zero cap in {err}"
        );
    }

    #[test]
    fn refuses_invalid_settings_with_clear_messages() {
        let bad_pct = "[quota]\nfive_hour_stop_pct = 140\n";
        let err = Config::parse(bad_pct).unwrap_err().to_string();
        assert!(
            err.contains("quota.five_hour_stop_pct"),
            "expected setting name in {err}"
        );

        let bad_server_margin = "[quota]\nserver_only_margin_pct = -1.0\n";
        let err = Config::parse(bad_server_margin).unwrap_err().to_string();
        assert!(
            err.contains("quota.server_only_margin_pct"),
            "expected setting name in {err}"
        );

        let bad_source = r#"[quota]
small_task_types = ["broken-check"]
"#;
        let err = Config::parse(bad_source).unwrap_err().to_string();
        for src in ["broken_check", "roadmap", "github_issue", "session"] {
            assert!(err.contains(src), "expected source {src} in {err}");
        }

        let bad_model = r#"[quota]
claude_standard_model = "claude-opus-4"
"#;
        let err = Config::parse(bad_model).unwrap_err().to_string();
        assert!(
            err.contains("Opus orchestrates only"),
            "expected Opus explanation in {err}"
        );

        let bad_small_model = r#"[quota]
claude_small_model = "OPUS-3"
"#;
        let err = Config::parse(bad_small_model).unwrap_err().to_string();
        assert!(
            err.contains("Opus orchestrates only"),
            "expected Opus explanation in {err}"
        );
    }

    #[test]
    fn refuses_empty_tier_model() {
        let bad = "[quota]\ncodex_small_model = \"\"\n";
        let err = Config::parse(bad).unwrap_err().to_string();
        assert!(
            err.contains("quota.codex_small_model cannot be empty"),
            "expected empty model error in {err}"
        );

        let bad_spaces = "[quota]\nclaude_small_model = \"   \"\n";
        let err_spaces = Config::parse(bad_spaces).unwrap_err().to_string();
        assert!(
            err_spaces.contains("quota.claude_small_model cannot be empty"),
            "expected empty model error in {err_spaces}"
        );
    }
}
