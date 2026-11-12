//! Domain: the `[dispatcher]` section of `config.toml`: the mode, the worker lines of D14 and
//! the worker models. Pure.

use serde::{Deserialize, Serialize};

use crate::dispatch::Mode;
use crate::quota::Provider;

/// The dispatcher's settings. The line numbers are the laptop's starting values (D14, A25).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatcherConfig {
    pub mode: Mode,
    pub hold_mem_used_pct: f64,
    pub shed_mem_used_pct: f64,
    pub emergency_mem_used_pct: f64,
    pub hold_disk_free_mib: u64,
    pub shed_disk_free_mib: u64,
    pub emergency_disk_free_mib: u64,
    /// Swap-in at or under this many pages a second counts as low.
    pub swap_in_low_pages_per_sec: f64,
    /// Swap-in above this many pages a second counts as high.
    pub swap_in_high_pages_per_sec: f64,
    /// Memory one worker is expected to take; sets the number of open slots.
    pub worker_mem_mib: u64,
    /// Models a worker may use, as "provider:model" (A21).
    pub worker_models: Vec<String>,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Propose,
            hold_mem_used_pct: 90.0,
            shed_mem_used_pct: 95.0,
            emergency_mem_used_pct: 98.0,
            hold_disk_free_mib: 4096,
            shed_disk_free_mib: 2048,
            emergency_disk_free_mib: 1024,
            swap_in_low_pages_per_sec: 10.0,
            swap_in_high_pages_per_sec: 100.0,
            worker_mem_mib: 512,
            worker_models: vec!["claude:sonnet".into()],
        }
    }
}

impl DispatcherConfig {
    /// Refuses lines that are out of range or out of order, and malformed worker models.
    /// The error starts with the key it is about.
    pub fn validate(&self) -> Result<(), String> {
        let pcts = [
            ("hold_mem_used_pct", self.hold_mem_used_pct),
            ("shed_mem_used_pct", self.shed_mem_used_pct),
            ("emergency_mem_used_pct", self.emergency_mem_used_pct),
        ];
        for (key, v) in pcts {
            if !(0.0..=100.0).contains(&v) {
                return Err(format!("dispatcher.{key}: must be within 0..=100, got {v}"));
            }
        }
        if self.hold_mem_used_pct >= self.shed_mem_used_pct {
            return Err(format!(
                "dispatcher.hold_mem_used_pct: must be below shed_mem_used_pct ({})",
                self.shed_mem_used_pct
            ));
        }
        if self.shed_mem_used_pct >= self.emergency_mem_used_pct {
            return Err(format!(
                "dispatcher.shed_mem_used_pct: must be below emergency_mem_used_pct ({})",
                self.emergency_mem_used_pct
            ));
        }
        if self.hold_disk_free_mib <= self.shed_disk_free_mib {
            return Err(format!(
                "dispatcher.hold_disk_free_mib: must be above shed_disk_free_mib ({})",
                self.shed_disk_free_mib
            ));
        }
        if self.shed_disk_free_mib <= self.emergency_disk_free_mib {
            return Err(format!(
                "dispatcher.shed_disk_free_mib: must be above emergency_disk_free_mib ({})",
                self.emergency_disk_free_mib
            ));
        }
        if self.swap_in_low_pages_per_sec > self.swap_in_high_pages_per_sec {
            return Err(format!(
                "dispatcher.swap_in_low_pages_per_sec: must not be above swap_in_high_pages_per_sec ({})",
                self.swap_in_high_pages_per_sec
            ));
        }
        if self.worker_mem_mib == 0 {
            return Err("dispatcher.worker_mem_mib: must be above 0".into());
        }
        for entry in &self.worker_models {
            parse_worker_model(entry).map_err(|e| format!("dispatcher.worker_models: {e}"))?;
        }
        Ok(())
    }

    /// The worker models as (provider, model) pairs; malformed entries are left out.
    pub fn models(&self) -> Vec<(Provider, String)> {
        self.worker_models
            .iter()
            .filter_map(|e| parse_worker_model(e).ok())
            .collect()
    }
}

/// "claude:sonnet" as (Claude, "sonnet").
pub fn parse_worker_model(entry: &str) -> Result<(Provider, String), String> {
    let mut parts = entry.split(':');
    let (Some(provider), Some(model), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("{entry:?} must be provider:model"));
    };
    let provider: Provider = serde_json::from_value(serde_json::Value::String(provider.into()))
        .map_err(|_| format!("{entry:?} names an unknown provider {provider:?}"))?;
    if model.is_empty() || model.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("{entry:?} has an empty or malformed model name"));
    }
    Ok((provider, model.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn auto_mode_parses() {
        let cfg = crate::config::Config::parse("[dispatcher]\nmode = \"auto\"\n").unwrap();
        assert_eq!(cfg.dispatcher.mode, Mode::Auto);
    }

    #[test]
    fn an_empty_file_proposes_with_the_claude_standard_model() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.dispatcher.mode, Mode::Propose);
        assert_eq!(cfg.dispatcher.worker_models, ["claude:sonnet"]);
    }

    #[test]
    fn lines_out_of_order_fail_on_one_line_naming_the_key() {
        let msg = Config::parse("[dispatcher]\nhold_mem_used_pct = 96\n")
            .unwrap_err()
            .to_string();
        assert_eq!(msg.lines().count(), 1, "{msg:?}");
        assert!(msg.starts_with("dispatcher.hold_mem_used_pct"), "{msg:?}");
        for bad in [
            "worker_mem_mib = 0",
            "worker_models = [\"claude\"]",
            "worker_models = [\"nope:x\"]",
            "shed_disk_free_mib = 9000",
            "swap_in_low_pages_per_sec = 500.0",
        ] {
            assert!(
                Config::parse(&format!("[dispatcher]\n{bad}\n")).is_err(),
                "{bad}"
            );
        }
    }
}
