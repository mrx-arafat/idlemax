//! Domain: model tiers and provider model mappings. Pure.

use serde::{Deserialize, Serialize};

use crate::quota::{Provider, QuotaConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Small,
    Standard,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Small => "small",
            Tier::Standard => "standard",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierPick {
    Run(Tier),
    NeedsYou,
}

/// 0 failed checks: Small when the task type is in `small_task_types`, else Standard.
/// 1 failed check: Standard. 2 or more: NeedsYou.
pub fn tier_for(task_type: &str, failed_checks: u32, cfg: &QuotaConfig) -> TierPick {
    match failed_checks {
        0 => {
            if cfg.small_task_types.iter().any(|t| t == task_type) {
                TierPick::Run(Tier::Small)
            } else {
                TierPick::Run(Tier::Standard)
            }
        }
        1 => TierPick::Run(Tier::Standard),
        _ => TierPick::NeedsYou,
    }
}

pub fn model_for(provider: Provider, tier: Tier, cfg: &QuotaConfig) -> Option<&str> {
    let model = match (provider, tier) {
        (Provider::Claude, Tier::Small) => cfg.claude_small_model.as_deref(),
        (Provider::Claude, Tier::Standard) => cfg.claude_standard_model.as_deref(),
        (Provider::Codex, Tier::Small) => cfg.codex_small_model.as_deref(),
        (Provider::Codex, Tier::Standard) => cfg.codex_standard_model.as_deref(),
        (Provider::Agy, Tier::Small) => cfg.agy_small_model.as_deref(),
        (Provider::Agy, Tier::Standard) => cfg.agy_standard_model.as_deref(),
        (Provider::Opencode, Tier::Small) => cfg.opencode_small_model.as_deref(),
        (Provider::Opencode, Tier::Standard) => cfg.opencode_standard_model.as_deref(),
        (Provider::Api, _) => None,
    };
    model.filter(|m| !m.trim().is_empty())
}

/// The setting that names this model, such as `quota.codex_small_model`; None for `api`.
pub fn model_key(provider: Provider, tier: Tier) -> Option<&'static str> {
    match (provider, tier) {
        (Provider::Claude, Tier::Small) => Some("quota.claude_small_model"),
        (Provider::Claude, Tier::Standard) => Some("quota.claude_standard_model"),
        (Provider::Codex, Tier::Small) => Some("quota.codex_small_model"),
        (Provider::Codex, Tier::Standard) => Some("quota.codex_standard_model"),
        (Provider::Agy, Tier::Small) => Some("quota.agy_small_model"),
        (Provider::Agy, Tier::Standard) => Some("quota.agy_standard_model"),
        (Provider::Opencode, Tier::Small) => Some("quota.opencode_small_model"),
        (Provider::Opencode, Tier::Standard) => Some("quota.opencode_standard_model"),
        (Provider::Api, _) => None,
    }
}

/// Every (provider, model) a tier key sets, small before standard.
pub fn tier_models(cfg: &QuotaConfig) -> Vec<(Provider, String)> {
    let mut models = Vec::new();
    for tier in [Tier::Small, Tier::Standard] {
        for provider in [
            Provider::Claude,
            Provider::Codex,
            Provider::Agy,
            Provider::Opencode,
        ] {
            if let Some(m) = model_for(provider, tier, cfg) {
                models.push((provider, m.to_string()));
            }
        }
    }
    models
}

/// The tier whose key holds `model` for this provider; Standard when both hold it, None for another name.
pub fn tier_of(provider: Provider, model: &str, cfg: &QuotaConfig) -> Option<Tier> {
    let standard = model_for(provider, Tier::Standard, cfg);
    let small = model_for(provider, Tier::Small, cfg);
    if standard == Some(model) {
        Some(Tier::Standard)
    } else if small == Some(model) {
        Some(Tier::Small)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_selection_follows_failure_count_and_task_type() {
        let cfg = QuotaConfig::default();
        assert_eq!(
            tier_for("broken_check", 0, &cfg),
            TierPick::Run(Tier::Small)
        );
        assert_eq!(tier_for("roadmap", 0, &cfg), TierPick::Run(Tier::Standard));
        assert_eq!(
            tier_for("broken_check", 1, &cfg),
            TierPick::Run(Tier::Standard)
        );
        assert_eq!(tier_for("roadmap", 1, &cfg), TierPick::Run(Tier::Standard));
        assert_eq!(tier_for("broken_check", 2, &cfg), TierPick::NeedsYou);
        assert_eq!(tier_for("roadmap", 2, &cfg), TierPick::NeedsYou);
    }

    #[test]
    fn tier_models_and_tier_of_reflect_configured_models() {
        let cfg = QuotaConfig::default();
        assert_eq!(
            tier_models(&cfg),
            vec![
                (Provider::Claude, "haiku".into()),
                (Provider::Claude, "sonnet".into())
            ]
        );
        assert_eq!(
            tier_of(Provider::Claude, "sonnet", &cfg),
            Some(Tier::Standard)
        );
        assert_eq!(tier_of(Provider::Claude, "haiku", &cfg), Some(Tier::Small));
        assert_eq!(tier_of(Provider::Claude, "opus", &cfg), None);
        assert_eq!(tier_of(Provider::Codex, "sonnet", &cfg), None);
    }

    #[test]
    fn model_for_and_model_key_map_correctly() {
        let cfg = QuotaConfig::default();
        assert_eq!(
            model_for(Provider::Claude, Tier::Small, &cfg),
            Some("haiku")
        );
        assert_eq!(
            model_for(Provider::Claude, Tier::Standard, &cfg),
            Some("sonnet")
        );
        assert_eq!(model_for(Provider::Codex, Tier::Small, &cfg), None);

        assert_eq!(
            model_key(Provider::Claude, Tier::Small),
            Some("quota.claude_small_model")
        );
        assert_eq!(
            model_key(Provider::Codex, Tier::Standard),
            Some("quota.codex_standard_model")
        );
        assert_eq!(model_key(Provider::Api, Tier::Small), None);

        let empty_cfg = QuotaConfig {
            codex_small_model: Some("   ".into()),
            codex_standard_model: Some("".into()),
            ..Default::default()
        };
        assert_eq!(model_for(Provider::Codex, Tier::Small, &empty_cfg), None);
        assert_eq!(model_for(Provider::Codex, Tier::Standard, &empty_cfg), None);
    }
}
