//! Quota ledger: probe snapshots per account window and rate-limit hits (adapter).

use idlemax_core::quota;
use idlemax_core::types::{QuotaSnapshot, UnixMs};
use rusqlite::{params, Connection, Row};

use super::{enum_col, enum_text, json_col, json_text, Store, StoreResult};

impl Store {
    /// Newest snapshot per account window, sorted by account.
    pub fn latest_quota(&self) -> StoreResult<Vec<QuotaSnapshot>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, quota_window, used_pct, resets_at_ms, source, at_ms
             FROM quota_snapshots AS s
             WHERE s.at_ms = (SELECT max(n.at_ms) FROM quota_snapshots AS n
                              WHERE n.account = s.account AND n.quota_window = s.quota_window)
             ORDER BY account, quota_window",
        )?;
        let rows = stmt.query_map([], snapshot_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Newest limit hit per account when it is at or after `since_ms`, sorted by account.
    pub fn limit_hits(&self, since_ms: UnixMs) -> StoreResult<Vec<(String, UnixMs)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, at_ms FROM rate_limits WHERE at_ms >= ?1 ORDER BY account",
        )?;
        let rows = stmt.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Remembers the newest usage limit or HTTP 429 seen for an AI account; the caller owns the
/// transaction.
pub(super) fn upsert_limit_hit(conn: &Connection, account: &str, at_ms: UnixMs) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO rate_limits (account, at_ms) VALUES (?1, ?2)
         ON CONFLICT (account) DO UPDATE SET at_ms = max(at_ms, excluded.at_ms)",
        params![account, at_ms],
    )?;
    Ok(())
}

pub(super) fn insert_snapshots(
    conn: &Connection,
    snaps: &[QuotaSnapshot],
    now: UnixMs,
) -> StoreResult<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO quota_snapshots (account, quota_window, at_ms, used_pct, resets_at_ms, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (account, quota_window, at_ms) DO NOTHING",
    )?;
    for s in snaps.iter().map(|s| quota::as_received(s, now)) {
        stmt.execute(params![
            s.account,
            json_text(&s.window)?,
            s.at_ms,
            s.used_pct,
            s.resets_at_ms,
            enum_text(&s.source)?
        ])?;
    }
    Ok(())
}

pub(super) fn snapshot_from_row(r: &Row<'_>) -> rusqlite::Result<QuotaSnapshot> {
    Ok(QuotaSnapshot {
        account: r.get(0)?,
        window: json_col(r, 1)?,
        used_pct: r.get(2)?,
        resets_at_ms: r.get(3)?,
        source: enum_col(r, 4)?,
        at_ms: r.get(5)?,
    })
}
