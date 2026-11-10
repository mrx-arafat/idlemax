//! Domain: the quota each AI account has left to spend before its window resets.

use serde::{Deserialize, Serialize};

use crate::quota::{QuotaView, WindowState};
use crate::types::{QuotaWindowKind, UnixMs};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountCapacity {
    pub account: String,
    /// Every rule allows idlemax to spend the account now.
    pub spendable: bool,
    /// Paused by `idlemax pause --account`.
    pub paused: bool,
    /// Percent of the quota left to spend above the reserve in the limiting window, whether or
    /// not idlemax may spend it this minute (see `spendable` and `why` for that). None when
    /// that window has no trusted data.
    pub headroom_pct: Option<f64>,
    /// The window that limits the account, and when it resets.
    pub window: Option<QuotaWindowKind>,
    pub resets_at_ms: Option<UnixMs>,
    /// Why idlemax may not spend it now: the first reason of the account-level verdict.
    pub why: Option<String>,
}

/// One entry per account, in the order the views list them.
pub fn accounts(views: &[QuotaView], paused_accounts: &[String]) -> Vec<AccountCapacity> {
    let mut names: Vec<&str> = Vec::new();
    for v in views {
        if !names.contains(&v.account.as_str()) {
            names.push(&v.account);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let rows: Vec<&QuotaView> = views.iter().filter(|v| v.account == name).collect();
            let limiting = limiting(&rows);
            let spendable = rows.iter().all(|v| v.spendable);
            AccountCapacity {
                account: name.to_string(),
                spendable,
                paused: paused_accounts.iter().any(|a| a == name),
                headroom_pct: limiting.filter(|v| trusted(v)).map(|v| v.headroom_pct),
                window: limiting.and_then(|v| v.window),
                resets_at_ms: limiting.and_then(|v| v.resets_at_ms),
                why: limiting
                    .filter(|_| !spendable)
                    .and_then(|v| v.reasons.first().cloned()),
            }
        })
        .collect()
}

fn trusted(v: &QuotaView) -> bool {
    matches!(v.state, WindowState::Active | WindowState::NotStarted)
}

/// A window nothing can be spent from until it resets or reports: no trusted data, or nothing
/// left above its reserve.
fn blocks(v: &QuotaView) -> bool {
    v.window.is_some() && (!trusted(v) || v.headroom_pct <= 0.0)
}

/// The window that limits the account. Every row of an account carries the same verdict, so the
/// verdict cannot say which window it is; the rows can. A blocking window limits it: one with
/// no reset time first (it clears when data arrives, not by waiting), else the one that
/// resets last, because the account stays shut until then. With no blocking window, the quota
/// left is what the window with the least headroom has.
fn limiting<'a>(rows: &[&'a QuotaView]) -> Option<&'a QuotaView> {
    let blocking: Vec<&QuotaView> = rows.iter().copied().filter(|v| blocks(v)).collect();
    if blocking.is_empty() {
        return rows
            .iter()
            .copied()
            .min_by(|a, b| a.headroom_pct.total_cmp(&b.headroom_pct));
    }
    blocking
        .iter()
        .copied()
        .find(|v| v.resets_at_ms.is_none())
        .or_else(|| blocking.iter().copied().max_by_key(|v| v.resets_at_ms))
}
