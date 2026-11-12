//! Domain: which AI accounts the dispatcher may use now, by the simple rule of
//! `docs/DISPATCHER.md` that stands in for Spec 3's Budget: the measuring-week spend under
//! 10 percent of the weekly quota and the 5-hour window under 60 percent (D10.4, A18). Pure.
//!
//! It ignores `QuotaView::spendable`: the old spend rules (final third of a window, the
//! interactive hold, one AI job per 5 hours) would shut an account the whole time the owner
//! works, against D4.

use serde::{Deserialize, Serialize};

use crate::quota::{window_minutes, Provider, QuotaConfig, QuotaView, WindowState};
use crate::types::QuotaWindowKind;

pub const MEASURING_WEEK_CAP_PCT: f64 = 10.0;
pub const FIVE_HOUR_STOP_PCT: f64 = 60.0;
const FIVE_HOURS_MIN: u32 = 300;

/// One account as the dispatcher sees it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountUse {
    pub account: String,
    pub provider: Provider,
    pub usable: bool,
    /// Why it is or is not usable.
    pub why: String,
    pub five_hour_used_pct: Option<f64>,
    pub weekly_used_pct: Option<f64>,
}

/// Every configured account, in config order. `spent_week_pct` is the measuring-week spend
/// per account, empty until Spec 2 records Runs (A7).
pub fn account_uses(
    cfg: &QuotaConfig,
    views: &[QuotaView],
    paused: &[String],
    spent_week_pct: &[(String, f64)],
) -> Vec<AccountUse> {
    cfg.accounts
        .iter()
        .map(|a| {
            let rows: Vec<&QuotaView> = views.iter().filter(|v| v.account == a.name).collect();
            let five = rows.iter().find(|v| is_five_hour(v.window)).copied();
            let weekly = rows
                .iter()
                .find(|v| v.window == Some(QuotaWindowKind::Weekly))
                .copied();
            let spent = spent_week_pct
                .iter()
                .find(|(n, _)| *n == a.name)
                .map_or(0.0, |(_, p)| *p);
            let five_used = match five.map(|v| v.state) {
                None | Some(WindowState::NotStarted) => Some(0.0),
                _ => five.and_then(|v| v.used_pct),
            };
            let block = blocker(a.provider, &a.name, &rows, five, weekly, paused, spent);
            AccountUse {
                account: a.name.clone(),
                provider: a.provider,
                usable: block.is_none(),
                why: block.unwrap_or_else(|| usable_text(five_used, weekly)),
                five_hour_used_pct: five_used,
                weekly_used_pct: weekly.and_then(|v| v.used_pct),
            }
        })
        .collect()
}

fn is_five_hour(window: Option<QuotaWindowKind>) -> bool {
    window.is_some_and(|w| w != QuotaWindowKind::Weekly && window_minutes(w) == FIVE_HOURS_MIN)
}

/// The first rule that stops the account, in the order of the spec.
fn blocker(
    provider: Provider,
    name: &str,
    rows: &[&QuotaView],
    five: Option<&QuotaView>,
    weekly: Option<&QuotaView>,
    paused: &[String],
    spent: f64,
) -> Option<String> {
    if matches!(provider, Provider::Agy | Provider::Api) {
        let shown = serde_json::to_value(provider)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        return Some(format!("no quota reading for {shown} yet"));
    }
    if paused.iter().any(|p| p == name) {
        return Some("paused by you".into());
    }
    if let Some(text) = rows.iter().find_map(|v| v.limited.clone()) {
        return Some(text);
    }
    match weekly.map(|v| v.state) {
        Some(WindowState::Active) => {}
        Some(state) => return Some(format!("weekly window: {}", state_text(state))),
        None => return Some("weekly window: no data".into()),
    }
    match five.map(|v| (v.state, v.used_pct)) {
        Some((WindowState::Stale, _)) => return Some("5-hour data is older than 6 h".into()),
        Some((WindowState::NoData | WindowState::ResetPending, _)) => {
            return Some("no 5-hour data".into())
        }
        Some((WindowState::Active, Some(used))) if used >= FIVE_HOUR_STOP_PCT => {
            return Some(format!(
                "5-hour window is {}% used, stops at {}%",
                pct(used),
                pct(FIVE_HOUR_STOP_PCT)
            ));
        }
        _ => {}
    }
    (spent >= MEASURING_WEEK_CAP_PCT).then(|| {
        format!(
            "idlemax has spent {}% of this week's quota",
            pct(MEASURING_WEEK_CAP_PCT)
        )
    })
}

fn state_text(state: WindowState) -> &'static str {
    match state {
        WindowState::Active => "active",
        WindowState::NotStarted => "not started",
        WindowState::Stale => "data is older than 6 h",
        WindowState::ResetPending => "reset, waiting for fresh data",
        WindowState::NoData => "no data",
    }
}

fn usable_text(five: Option<f64>, weekly: Option<&QuotaView>) -> String {
    let week = weekly
        .and_then(|v| v.used_pct)
        .map_or_else(|| "unknown".into(), |u| format!("{}%", pct(u)));
    let five = five.map_or_else(|| "unknown".into(), |u| format!("{}%", pct(u)));
    format!("usable: 5-hour window {five} used, weekly {week} used")
}

fn pct(v: f64) -> String {
    let text = format!("{v:.1}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::{rolling_5h, AccountConfig};

    fn cfg(provider: Provider) -> QuotaConfig {
        QuotaConfig {
            accounts: vec![AccountConfig {
                name: "a".into(),
                provider,
                ..AccountConfig::default()
            }],
        }
    }

    fn row(window: QuotaWindowKind, state: WindowState, used: f64) -> QuotaView {
        QuotaView {
            account: "a".into(),
            window: Some(window),
            state,
            used_pct: Some(used),
            ..Default::default()
        }
    }

    fn rows(five: f64) -> Vec<QuotaView> {
        vec![
            row(rolling_5h(), WindowState::Active, five),
            row(QuotaWindowKind::Weekly, WindowState::Active, 40.0),
        ]
    }

    fn usable(provider: Provider, views: &[QuotaView], spent: f64) -> (bool, String) {
        let uses = account_uses(&cfg(provider), views, &[], &[("a".into(), spent)]);
        (uses[0].usable, uses[0].why.clone())
    }

    #[test]
    fn the_simple_rule_stops_at_sixty_percent_of_five_hours_and_ten_percent_of_the_week() {
        assert!(usable(Provider::Claude, &rows(59.9), 0.0).0);
        let (ok, why) = usable(Provider::Claude, &rows(60.0), 0.0);
        assert!(!ok && why.contains("60%"), "{why}");
        assert!(!usable(Provider::Claude, &rows(10.0), 10.0).0);
        assert!(!usable(Provider::Agy, &rows(10.0), 0.0).0);
        let mut limited = rows(10.0);
        limited[0].limited = Some("usage limit hit 5m ago".into());
        assert_eq!(
            usable(Provider::Claude, &limited, 0.0),
            (false, "usage limit hit 5m ago".into())
        );
        let mut no_data = rows(10.0);
        no_data[0].state = WindowState::NoData;
        assert!(!usable(Provider::Claude, &no_data, 0.0).0);
        let weekly_only = vec![row(QuotaWindowKind::Weekly, WindowState::Active, 40.0)];
        assert!(usable(Provider::Codex, &weekly_only, 0.0).0);
    }
}
