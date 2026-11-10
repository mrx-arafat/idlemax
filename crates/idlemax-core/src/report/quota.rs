//! Domain: AI quota spent against quota that expired unused, per window cycle (pure).
//!
//! A cycle is one run of snapshots that report the same reset time. The most a cycle was
//! seen to use is a lower bound of what was spent, so its unused share is an upper bound:
//! use between the last probe and the reset is not seen.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Window;
use crate::quota::window_label;
use crate::types::{QuotaSnapshot, QuotaWindowKind, UnixMs};

/// Reset times closer than this belong to the same cycle (probes round them differently).
const SAME_RESET_MS: i64 = 5 * 60_000;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QuotaSummary {
    /// An AI account is configured. When false there is nothing to report, by design.
    pub configured: bool,
    /// The earliest time the cycles below are counted from: the period's start, or later when
    /// the coordinator keeps quota snapshots for less time than the period covers.
    pub counted_since_ms: UnixMs,
    /// Cycles whose reset fell inside the period, so what they did not use is gone.
    pub expired: Vec<QuotaCycle>,
    /// Cycles still running at the end of the period.
    pub open: Vec<QuotaCycle>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaCycle {
    pub account: String,
    pub window: QuotaWindowKind,
    /// `5h`, `weekly`.
    pub window_label: String,
    /// The most percent of the window seen used in the cycle.
    pub peak_used_pct: f64,
    /// 100 minus the peak: what the cycle left unspent.
    pub unused_pct: f64,
    pub resets_at_ms: UnixMs,
}

pub(super) fn summarize(
    snapshots: &[QuotaSnapshot],
    configured: bool,
    window: &Window,
    kept_since_ms: UnixMs,
) -> QuotaSummary {
    let counted_since_ms = window.since_ms.max(kept_since_ms);
    let mut summary = QuotaSummary {
        configured,
        counted_since_ms,
        ..QuotaSummary::default()
    };
    if !configured {
        return summary;
    }
    let mut by_window: BTreeMap<(&str, String), (QuotaWindowKind, Vec<&QuotaSnapshot>)> =
        BTreeMap::new();
    for s in snapshots {
        let key = (s.account.as_str(), window_label(s.window));
        by_window
            .entry(key)
            .or_insert((s.window, Vec::new()))
            .1
            .push(s);
    }
    for ((account, _), (kind, mut snaps)) in by_window {
        snaps.sort_by_key(|s| s.at_ms);
        for cycle in cycles(account, kind, &snaps) {
            if cycle.resets_at_ms > window.until_ms {
                summary.open.push(cycle);
            } else if cycle.resets_at_ms >= counted_since_ms {
                summary.expired.push(cycle);
            }
        }
    }
    summary
}

/// The cycles one account window went through, oldest first. Snapshots with no reset time
/// (a rolling window that is not running) belong to no cycle.
fn cycles(account: &str, kind: QuotaWindowKind, snaps: &[&QuotaSnapshot]) -> Vec<QuotaCycle> {
    let mut out: Vec<QuotaCycle> = Vec::new();
    for s in snaps {
        let Some(resets_at_ms) = s.resets_at_ms else {
            continue;
        };
        let peak = s.used_pct.clamp(0.0, 100.0);
        match out.last_mut() {
            Some(c) if (resets_at_ms - c.resets_at_ms).abs() <= SAME_RESET_MS => {
                c.peak_used_pct = c.peak_used_pct.max(peak);
                c.resets_at_ms = c.resets_at_ms.max(resets_at_ms);
            }
            _ => out.push(QuotaCycle {
                account: account.to_string(),
                window: kind,
                window_label: window_label(kind),
                peak_used_pct: peak,
                unused_pct: 0.0,
                resets_at_ms,
            }),
        }
    }
    for c in &mut out {
        c.unused_pct = 100.0 - c.peak_used_pct;
    }
    out
}
