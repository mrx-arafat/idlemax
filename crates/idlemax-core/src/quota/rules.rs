//! Domain: evaluates account windows and the AI spend rules from PLAN.md (pure; time is a parameter).

use super::{
    window_label, window_minutes, AccountConfig, QuotaConfig, QuotaView, WindowConfig, WindowState,
    MINUTE_MS,
};
use crate::types::{QuotaSnapshot, QuotaWindowKind, UnixMs};

/// Hold an account while its CLI was used interactively this recently.
pub const INTERACTIVE_HOLD_MS: i64 = 30 * MINUTE_MS;
/// Snapshots older than this are not trusted.
pub const SNAPSHOT_MAX_AGE_MS: i64 = 6 * 60 * MINUTE_MS;
/// The weekly-reset rule and the final-day reserve apply this close to a weekly reset.
pub const FINAL_DAY_MS: i64 = 24 * 60 * MINUTE_MS;
/// One AI job per account per 5 hours (03-design-options.md MVP default). Headroom goes
/// stale after a job because `claude -p` runs never refresh the statusline capture.
pub const AI_JOB_SPAN_MS: i64 = 300 * MINUTE_MS;

const LIMIT_PHRASES: [&str; 8] = [
    "usage limit",
    "session limit",
    "weekly limit",
    "hit your limit",
    "rate limit",
    "rate_limit",
    "too many requests",
    "quota exceeded",
];

/// Inputs for the spend decision, gathered by the coordinator from every device.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpendContext<'a> {
    pub now_ms: UnixMs,
    /// Snapshots of any account and age; the newest per account window wins.
    pub snapshots: &'a [QuotaSnapshot],
    /// (cli name, last interactive use).
    pub interactive_use: &'a [(String, UnixMs)],
    /// Accounts with an AI job holding a lease anywhere.
    pub leased_accounts: &'a [String],
    /// (account, time) a usage limit or HTTP 429 was seen.
    pub limit_hits: &'a [(String, UnixMs)],
    /// (account, start time) of recent AI jobs.
    pub ai_job_starts: &'a [(String, UnixMs)],
}

struct Window<'a> {
    cfg: WindowConfig,
    snap: Option<&'a QuotaSnapshot>,
    state: WindowState,
}

/// One row per account window, each carrying the account verdict.
pub fn views(cfg: &QuotaConfig, ctx: &SpendContext<'_>) -> Vec<QuotaView> {
    cfg.accounts
        .iter()
        .flat_map(|a| account_views(a, ctx))
        .collect()
}

/// True only when every rule allows spending `account` now. Unknown accounts never spend.
/// The caller still checks that the device lists the account in `ai_accounts`.
pub fn spendable(cfg: &QuotaConfig, account: &str, ctx: &SpendContext<'_>) -> bool {
    cfg.account(account)
        .is_some_and(|a| verdict(a, &windows(a, ctx), ctx).0)
}

/// Classifies CLI output that reports a usage limit or HTTP 429.
pub fn is_limit_message(text: &str) -> bool {
    let lower = text.to_lowercase();
    LIMIT_PHRASES.iter().any(|p| lower.contains(p))
}

fn account_views(account: &AccountConfig, ctx: &SpendContext<'_>) -> Vec<QuotaView> {
    let windows = windows(account, ctx);
    let (spendable, reasons) = verdict(account, &windows, ctx);
    let limited = windows
        .first()
        .and_then(|shortest| limit_block(account, shortest, ctx));
    let base = QuotaView {
        account: account.name.clone(),
        provider: account.provider,
        spendable,
        reasons,
        limited,
        at_ms: ctx.now_ms,
        ..QuotaView::default()
    };
    if windows.is_empty() {
        return vec![base];
    }
    windows.iter().map(|w| row(w, &base, ctx.now_ms)).collect()
}

fn row(w: &Window<'_>, base: &QuotaView, now: UnixMs) -> QuotaView {
    let reserve = reserve_pct(w, now);
    let used = match w.state {
        WindowState::Active | WindowState::Stale => w.snap.map(|s| s.used_pct),
        WindowState::NotStarted => Some(0.0),
        WindowState::ResetPending | WindowState::NoData => None,
    };
    let trusted = matches!(w.state, WindowState::Active | WindowState::NotStarted);
    QuotaView {
        window: Some(w.cfg.kind),
        state: w.state,
        used_pct: used,
        reserve_pct: reserve,
        headroom_pct: match used {
            Some(u) if trusted => (100.0 - reserve - u).max(0.0),
            _ => 0.0,
        },
        resets_at_ms: w.snap.and_then(|s| s.resets_at_ms).filter(|r| *r > now),
        snapshot_ms: w.snap.map(|s| s.at_ms),
        source: w.snap.map(|s| s.source),
        ..base.clone()
    }
}

/// Configured windows plus any reported ones, shortest first.
fn windows<'a>(account: &AccountConfig, ctx: &SpendContext<'a>) -> Vec<Window<'a>> {
    let mut cfgs = account.effective_windows();
    for s in ctx.snapshots.iter().filter(|s| s.account == account.name) {
        if !cfgs.iter().any(|c| c.kind == s.window) {
            cfgs.push(WindowConfig::default_for(s.window));
        }
    }
    cfgs.sort_by_key(|c| window_minutes(c.kind));
    cfgs.into_iter()
        .map(|cfg| {
            let snap = newest(ctx.snapshots, &account.name, cfg.kind);
            let state = window_state(cfg.kind, snap, ctx.now_ms);
            Window { cfg, snap, state }
        })
        .collect()
}

fn newest<'a>(
    snaps: &'a [QuotaSnapshot],
    account: &str,
    kind: QuotaWindowKind,
) -> Option<&'a QuotaSnapshot> {
    snaps
        .iter()
        .filter(|s| s.account == account && s.window == kind)
        .max_by_key(|s| s.at_ms)
}

fn window_state(kind: QuotaWindowKind, snap: Option<&QuotaSnapshot>, now: UnixMs) -> WindowState {
    let rolling = matches!(kind, QuotaWindowKind::Rolling { .. });
    let Some(s) = snap else {
        // Claude omits a 5-hour window that has not started.
        return if rolling {
            WindowState::NotStarted
        } else {
            WindowState::NoData
        };
    };
    if s.resets_at_ms.is_some_and(|r| r <= now) {
        return if rolling {
            WindowState::NotStarted
        } else {
            WindowState::ResetPending
        };
    }
    if now - s.at_ms > SNAPSHOT_MAX_AGE_MS {
        return WindowState::Stale;
    }
    WindowState::Active
}

fn reserve_pct(w: &Window<'_>, now: UnixMs) -> f64 {
    match (
        w.cfg.final_day_reserve_pct,
        weekly_reset_at(w, now).is_some(),
    ) {
        (Some(final_day), true) => final_day,
        _ => w.cfg.reserve_pct,
    }
}

/// Reset time of a weekly window with fresh data that resets within 24 h.
fn weekly_reset_at(w: &Window<'_>, now: UnixMs) -> Option<UnixMs> {
    if w.cfg.kind != QuotaWindowKind::Weekly || w.state != WindowState::Active {
        return None;
    }
    w.snap
        .and_then(|s| s.resets_at_ms)
        .filter(|r| *r - now <= FINAL_DAY_MS)
}

/// (spendable, reasons). Reasons explain the timing pass when spendable, else every block.
fn verdict(
    account: &AccountConfig,
    windows: &[Window<'_>],
    ctx: &SpendContext<'_>,
) -> (bool, Vec<String>) {
    let now = ctx.now_ms;
    let Some(shortest) = windows.first() else {
        return (
            false,
            vec!["no quota windows configured or reported".into()],
        );
    };
    let mut blocks: Vec<String> = windows
        .iter()
        .filter_map(|w| window_block(w, now))
        .collect();
    let timing = timing(shortest, windows, now);
    if let Err(reason) = &timing {
        blocks.push(reason.clone());
    }
    blocks.extend(interactive_block(account, ctx));
    if ctx.leased_accounts.contains(&account.name) {
        blocks.push("another AI job holds this account's lease".into());
    }
    blocks.extend(limit_block(account, shortest, ctx));
    blocks.extend(job_span_block(account, ctx));
    match timing {
        Ok(reason) if blocks.is_empty() => (true, vec![reason]),
        _ => (false, blocks),
    }
}

fn window_block(w: &Window<'_>, now: UnixMs) -> Option<String> {
    let label = window_label(w.cfg.kind);
    match (w.state, w.snap) {
        (WindowState::Active, Some(s)) => {
            let reserve = reserve_pct(w, now);
            (s.used_pct >= 100.0 - reserve).then(|| {
                format!(
                    "{label}: {:.0}% used, {reserve:.0}% reserve leaves no headroom",
                    s.used_pct
                )
            })
        }
        (WindowState::Active | WindowState::NotStarted, _) => None,
        (WindowState::Stale, _) => Some(format!("{label}: data older than 6 h")),
        (WindowState::ResetPending, _) => {
            Some(format!("{label}: window reset, waiting for fresh data"))
        }
        (WindowState::NoData, _) => Some(format!("{label}: no data")),
    }
}

/// Spend only in the final third of the shortest window, or within 24 h of a weekly reset.
fn timing(shortest: &Window<'_>, windows: &[Window<'_>], now: UnixMs) -> Result<String, String> {
    let label = window_label(shortest.cfg.kind);
    let reset = shortest.snap.and_then(|s| s.resets_at_ms);
    if let (WindowState::Active, Some(r)) = (shortest.state, reset) {
        let final_third_ms = i64::from(window_minutes(shortest.cfg.kind)) * MINUTE_MS / 3;
        if now >= r - final_third_ms {
            return Ok(format!(
                "{label} window in its final third, resets in {}",
                duration(r - now)
            ));
        }
    }
    if let Some(r) = windows.iter().find_map(|w| weekly_reset_at(w, now)) {
        return Ok(format!("weekly window resets in {}", duration(r - now)));
    }
    Err(match shortest.state {
        WindowState::Active if reset.is_some() => {
            format!("{label} window not in its final third yet")
        }
        WindowState::NotStarted => format!("{label} window not started"),
        _ => format!("{label} window timing unknown"),
    })
}

fn interactive_block(account: &AccountConfig, ctx: &SpendContext<'_>) -> Option<String> {
    let cli = account.provider.cli()?;
    let last = ctx
        .interactive_use
        .iter()
        .filter(|(name, _)| name == cli)
        .map(|(_, t)| *t)
        .max()?;
    let ago = ctx.now_ms - last;
    (ago < INTERACTIVE_HOLD_MS).then(|| format!("{cli} used interactively {} ago", duration(ago)))
}

/// "No limit seen this window": a hit blocks until the shortest window that saw it resets.
/// Without a known reset it blocks for one window length after the hit.
fn limit_block(
    account: &AccountConfig,
    shortest: &Window<'_>,
    ctx: &SpendContext<'_>,
) -> Option<String> {
    let hit = ctx
        .limit_hits
        .iter()
        .filter(|(a, _)| *a == account.name)
        .map(|(_, t)| *t)
        .max()?;
    let len = i64::from(window_minutes(shortest.cfg.kind)) * MINUTE_MS;
    let until = match shortest.snap.and_then(|s| s.resets_at_ms) {
        Some(r) if hit < r - len => return None,
        Some(r) if hit < r => r,
        _ => hit + len,
    };
    (ctx.now_ms < until).then(|| {
        format!(
            "usage limit hit {} ago, blocked for {} more",
            duration(ctx.now_ms - hit),
            duration(until - ctx.now_ms)
        )
    })
}

fn job_span_block(account: &AccountConfig, ctx: &SpendContext<'_>) -> Option<String> {
    let recent = ctx
        .ai_job_starts
        .iter()
        .any(|(a, t)| *a == account.name && ctx.now_ms - t < AI_JOB_SPAN_MS);
    recent.then(|| "an AI job already started on this account in the last 5 h".to_string())
}

/// "3d 4h", "2h 5m", "42m".
pub(super) fn duration(ms: i64) -> String {
    let mins = ms.max(0) / MINUTE_MS;
    let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests;
