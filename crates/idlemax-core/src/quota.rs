//! Domain: AI quota accounts, windows and the spendable rules (pure; time is a parameter).
//!
//! Numbers come from 03-design-options.md and are estimates to tune after a week of data.

pub mod budget;
mod config;
pub mod cost;
pub mod owner_use;
mod rules;
pub mod spend;
pub mod surplus;
pub mod tier;

pub use config::{QuotaConfig, QuotaConfigError};
pub use rules::{is_limit_message, spendable, views, SpendContext, SNAPSHOT_MAX_AGE_MS};
pub use tier::{model_for, model_key, tier_for, tier_models, tier_of, Tier, TierPick};

use serde::{Deserialize, Serialize};

use crate::types::{ProbeSource, QuotaSnapshot, QuotaWindowKind, UnixMs};

pub const MINUTE_MS: i64 = 60_000;
/// Reset times closer than this belong to the same cycle (probes round them differently).
pub const SAME_RESET_MS: i64 = 5 * MINUTE_MS;
pub const WEEK_MINUTES: u32 = 7 * 24 * 60;
pub const ROLLING_RESERVE_PCT: f64 = 40.0;
pub const WEEKLY_RESERVE_PCT: f64 = 30.0;
/// Weekly reserve in the last 24 h before the weekly reset.
pub const WEEKLY_FINAL_DAY_RESERVE_PCT: f64 = 15.0;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Claude,
    Codex,
    Agy,
    Opencode,
    Api,
}

impl Provider {
    /// CLI name used as the `interactive_use` key; API accounts have no CLI.
    pub fn cli(self) -> Option<&'static str> {
        match self {
            Provider::Claude => Some("claude"),
            Provider::Codex => Some("codex"),
            Provider::Agy => Some("agy"),
            Provider::Opencode => Some("opencode"),
            Provider::Api => None,
        }
    }

    fn default_probe(self) -> ProbeSource {
        match self {
            Provider::Claude => ProbeSource::ClaudeStatusline,
            Provider::Codex => ProbeSource::CodexSessions,
            Provider::Agy | Provider::Opencode | Provider::Api => ProbeSource::Manual,
        }
    }

    /// Windows assumed when the config lists none. Codex Pro reports only a weekly
    /// window; a reported 5-hour window is added at evaluation time.
    fn default_windows(self) -> Vec<WindowConfig> {
        match self {
            Provider::Claude | Provider::Agy => {
                vec![
                    WindowConfig::default_for(rolling_5h()),
                    WindowConfig::default_for(QuotaWindowKind::Weekly),
                ]
            }
            Provider::Codex => vec![WindowConfig::default_for(QuotaWindowKind::Weekly)],
            Provider::Opencode | Provider::Api => Vec::new(),
        }
    }
}

/// `[[quota.accounts]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountConfig {
    pub name: String,
    pub provider: Provider,
    /// Empty means the provider defaults.
    pub windows: Vec<WindowConfig>,
    /// None means the provider default (claude_statusline, codex_sessions, manual).
    pub probe: Option<ProbeSource>,
    /// Capture file, sessions dir or manual file; None means the probe's default path.
    pub probe_path: Option<String>,
    /// Runs allowed per 24-hour day on providers whose quota cannot be read (agy, opencode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_cap_runs: Option<u32>,
}

impl AccountConfig {
    pub fn effective_probe(&self) -> ProbeSource {
        self.probe.unwrap_or_else(|| self.provider.default_probe())
    }

    pub fn effective_windows(&self) -> Vec<WindowConfig> {
        if self.windows.is_empty() {
            self.provider.default_windows()
        } else {
            self.windows.clone()
        }
    }
}

/// TOML: `{ kind = "rolling", minutes = 300, reserve_pct = 40 }` or `{ kind = "weekly", reserve_pct = 30 }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowConfig {
    #[serde(flatten)]
    pub kind: QuotaWindowKind,
    /// Percent kept back for interactive use.
    pub reserve_pct: f64,
    /// Weekly windows: reserve in the last 24 h before reset.
    #[serde(default)]
    pub final_day_reserve_pct: Option<f64>,
}

impl WindowConfig {
    pub fn default_for(kind: QuotaWindowKind) -> Self {
        match kind {
            QuotaWindowKind::Rolling { .. } => Self {
                kind,
                reserve_pct: ROLLING_RESERVE_PCT,
                final_day_reserve_pct: None,
            },
            QuotaWindowKind::Weekly => Self {
                kind,
                reserve_pct: WEEKLY_RESERVE_PCT,
                final_day_reserve_pct: Some(WEEKLY_FINAL_DAY_RESERVE_PCT),
            },
        }
    }

    pub(crate) fn validate(&self, account: &str) -> Result<(), QuotaConfigError> {
        if self.kind == (QuotaWindowKind::Rolling { minutes: 0 }) {
            return Err(QuotaConfigError::ZeroMinutes(account.to_string()));
        }
        let pcts = [
            ("reserve_pct", Some(self.reserve_pct)),
            ("final_day_reserve_pct", self.final_day_reserve_pct),
        ];
        for (field, value) in pcts {
            if let Some(v) = value.filter(|v| !(0.0..=100.0).contains(v)) {
                return Err(QuotaConfigError::PercentOutOfRange {
                    account: account.to_string(),
                    field,
                    value: v,
                });
            }
        }
        Ok(())
    }
}

pub fn rolling_5h() -> QuotaWindowKind {
    QuotaWindowKind::Rolling { minutes: 300 }
}

/// Window length in minutes; weekly is 7 days.
pub fn window_minutes(kind: QuotaWindowKind) -> u32 {
    match kind {
        QuotaWindowKind::Rolling { minutes } => minutes,
        QuotaWindowKind::Weekly => WEEK_MINUTES,
    }
}

/// Maps a provider-reported window length to a kind (Codex `window_minutes`).
pub fn window_from_minutes(minutes: u32) -> QuotaWindowKind {
    if minutes >= WEEK_MINUTES {
        QuotaWindowKind::Weekly
    } else {
        QuotaWindowKind::Rolling { minutes }
    }
}

/// Short display name: "5h", "90m", "weekly".
pub fn window_label(kind: QuotaWindowKind) -> String {
    match kind {
        QuotaWindowKind::Weekly => "weekly".into(),
        QuotaWindowKind::Rolling { minutes } if minutes % 60 == 0 => format!("{}h", minutes / 60),
        QuotaWindowKind::Rolling { minutes } => format!("{minutes}m"),
    }
}

/// How much the evaluator trusts a window's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    /// Fresh snapshot, window running.
    Active,
    /// Rolling window not running (none reported or its reset passed): nothing used.
    NotStarted,
    /// Newest snapshot is older than the freshness limit.
    Stale,
    /// Weekly window reset since the last snapshot; waiting for fresh data.
    ResetPending,
    #[default]
    NoData,
}

/// GET /v1/quota row: one account window. `spendable` and `reasons` are the
/// account-level verdict, repeated on every row of that account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaView {
    pub account: String,
    pub provider: Provider,
    /// None only on the single row of an account with no windows.
    pub window: Option<QuotaWindowKind>,
    pub state: WindowState,
    pub used_pct: Option<f64>,
    /// Reserve in force now (the weekly final-day reserve applies near reset).
    pub reserve_pct: f64,
    /// Spendable percent above the reserve; 0 when the data is not trusted.
    pub headroom_pct: f64,
    pub resets_at_ms: Option<UnixMs>,
    pub snapshot_ms: Option<UnixMs>,
    pub source: Option<ProbeSource>,
    pub spendable: bool,
    /// Why the account is or is not spendable.
    pub reasons: Vec<String>,
    /// A usage limit or HTTP 429 was seen in the current window: the account rests until that
    /// window resets. Repeated on every row of the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited: Option<String>,
    /// Evaluation time.
    pub at_ms: UnixMs,
}

/// A snapshot as the coordinator stores it: never dated after the coordinator's `now`. The
/// newest snapshot per window wins, so one capture from a device whose clock ran ahead would
/// otherwise hide every real snapshot until that future date.
pub fn as_received(snap: &QuotaSnapshot, now: UnixMs) -> QuotaSnapshot {
    QuotaSnapshot {
        at_ms: snap.at_ms.min(now),
        ..snap.clone()
    }
}

#[cfg(test)]
mod received_tests {
    use super::{as_received, rolling_5h, QuotaSnapshot};
    use crate::types::ProbeSource;

    #[test]
    fn a_snapshot_from_a_clock_ahead_is_dated_now_so_the_next_real_one_wins() {
        const NOW: i64 = 1_800_000_000_000;
        let snap = |at_ms, used_pct| QuotaSnapshot {
            account: "claude".into(),
            window: rolling_5h(),
            used_pct,
            resets_at_ms: None,
            source: ProbeSource::Manual,
            at_ms,
        };
        let future = as_received(&snap(NOW + 365 * 86_400_000, 10.0), NOW);
        assert_eq!(future.at_ms, NOW);
        let real = as_received(&snap(NOW + 30_000, 85.0), NOW + 30_000);
        assert!(real.at_ms > future.at_ms, "the next real snapshot is newer");
        assert_eq!(as_received(&snap(NOW - 5, 50.0), NOW).at_ms, NOW - 5);
    }
}
