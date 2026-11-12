//! Domain: the owner's hold on the weekly window and the surplus idlemax may spend (D10.2).
//! Pure; every figure is percent of the weekly window.

use super::owner_use::{OwnerUse, DAY_MS};
use super::QuotaConfig;
use crate::types::UnixMs;

/// Closed days after which the owner's own numbers replace the reserve.
pub const KNOWN_WEEK: u32 = 7;

/// What the owner keeps, and whether the learned use or the reserve set it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hold {
    pub pct: f64,
    /// True when the reserve floor (or a server-only override) set the hold.
    pub reserve: bool,
}

/// The owner's hold: the learned use until the reset, or, while fewer than 7 days are known,
/// the reserve minus what the owner already used this week, whichever is larger. With an
/// override (a server-only account) the hold is that figure minus the owner's use, and
/// nothing is learned.
pub fn hold(
    cfg: &QuotaConfig,
    owner: &OwnerUse,
    owner_week_pct: f64,
    resets_at_ms: UnixMs,
    now_ms: UnixMs,
    override_pct: Option<f64>,
) -> Hold {
    if let Some(reserve) = override_pct {
        return Hold {
            pct: (reserve - owner_week_pct).max(0.0),
            reserve: true,
        };
    }
    let days = (resets_at_ms - now_ms).max(0) as f64 / DAY_MS as f64;
    let learned = owner.usual_daily_pct.unwrap_or(0.0) * days;
    let floor = if owner.days_known < KNOWN_WEEK {
        (cfg.owner_reserve_pct - owner_week_pct).max(0.0)
    } else {
        0.0
    };
    Hold {
        pct: learned.max(floor),
        reserve: floor > learned,
    }
}

/// What would expire unused: 100 minus the weekly reading, the hold and the safety margin.
pub fn surplus(cfg: &QuotaConfig, weekly_used_pct: f64, hold: &Hold) -> f64 {
    (100.0 - weekly_used_pct - hold.pct - cfg.safety_margin_pct).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_override_holds_its_figure_minus_the_owners_use() {
        let cfg = QuotaConfig::default();
        let known = OwnerUse {
            days_known: 7,
            usual_daily_pct: Some(9.0),
        };
        let h = hold(&cfg, &known, 0.0, 3 * DAY_MS, 0, Some(5.0));
        assert_eq!(h.pct, 5.0);
    }
}
