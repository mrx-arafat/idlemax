//! Device registry, last heartbeat state and per-minute metrics history (adapter).

use std::collections::BTreeMap;

use idlemax_core::control::{device_online, offline_transition};
use idlemax_core::protocol::{DeviceView, Heartbeat};
use idlemax_core::report::GuardState;
use idlemax_core::types::{ControlScope, DeviceInfo, EventKind, UnixMs};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{
    enum_text, json_col, json_text, opt_json_col, u64_to_sql, Store, StoreError, StoreResult,
};

const MINUTE_MS: i64 = 60_000;

/// The existing info JSON stays readable by older coordinators as plain DeviceInfo.
#[derive(Serialize, Deserialize)]
struct StoredDevice {
    #[serde(flatten)]
    info: DeviceInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keep_awake: Option<String>,
}

const UPSERT_DEVICE: &str = "
INSERT INTO devices (id, info, first_seen_ms, last_seen_ms, metrics, guard, running,
                     interactive_use, local_paused, online, room)
VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)
ON CONFLICT (id) DO UPDATE SET
  info = excluded.info, last_seen_ms = excluded.last_seen_ms, metrics = excluded.metrics,
  guard = excluded.guard, running = excluded.running,
  interactive_use = excluded.interactive_use, local_paused = excluded.local_paused,
  online = 1, room = excluded.room";

// SET expressions read the row before the update, so `samples` is the old count.
const FOLD_MINUTE: &str = "
INSERT INTO metrics_minute (device, minute_ms, samples, cpu_busy_pct_avg, load_avg_1m_max,
  mem_free_pct_min, mem_available_bytes_min, swap_used_bytes_max, disk_free_bytes_min,
  user_idle_secs_min, on_ac_power, running_jobs_max, guard_severity_max)
VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT (device, minute_ms) DO UPDATE SET
  samples = samples + 1,
  cpu_busy_pct_avg = (cpu_busy_pct_avg * samples + excluded.cpu_busy_pct_avg) / (samples + 1),
  load_avg_1m_max = max(load_avg_1m_max, excluded.load_avg_1m_max),
  mem_free_pct_min = min(mem_free_pct_min, excluded.mem_free_pct_min),
  mem_available_bytes_min = min(mem_available_bytes_min, excluded.mem_available_bytes_min),
  swap_used_bytes_max = max(swap_used_bytes_max, excluded.swap_used_bytes_max),
  disk_free_bytes_min = min(disk_free_bytes_min, excluded.disk_free_bytes_min),
  user_idle_secs_min = coalesce(min(user_idle_secs_min, excluded.user_idle_secs_min),
                                user_idle_secs_min, excluded.user_idle_secs_min),
  on_ac_power = coalesce(min(on_ac_power, excluded.on_ac_power),
                         on_ac_power, excluded.on_ac_power),
  running_jobs_max = max(running_jobs_max, excluded.running_jobs_max),
  guard_severity_max = max(guard_severity_max, excluded.guard_severity_max)";

impl Store {
    /// Stores the device's latest state, folds its metrics into the current minute and
    /// records its quota snapshots. Returns the previous `last_seen_ms` (None when new).
    pub fn record_heartbeat(&mut self, hb: &Heartbeat, now: UnixMs) -> StoreResult<Option<UnixMs>> {
        let id = &hb.device.id;
        if id.trim().is_empty() {
            return Err(StoreError::Invalid("heartbeat device id is empty".into()));
        }
        let max_devices = self.caps.devices;
        let offline_after_ms = self.offline_after_ms;
        let max_spans = self.caps.guard_spans;
        let tx = self.conn.transaction()?;
        let previous = tx
            .query_row(
                "SELECT last_seen_ms FROM devices WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if previous.is_none() {
            let known: i64 = tx.query_row("SELECT count(*) FROM devices", [], |r| r.get(0))?;
            if known >= max_devices {
                return Err(StoreError::LimitReached {
                    what: "devices",
                    limit: max_devices,
                    hint: "a device silent for 30 days is forgotten",
                });
            }
        }
        tx.prepare_cached(UPSERT_DEVICE)?.execute(params![
            id,
            json_text(&StoredDevice {
                info: hb.device.clone(),
                keep_awake: hb.keep_awake.clone()
            })?,
            now,
            json_text(&hb.metrics)?,
            json_text(&hb.guard)?,
            json_text(&hb.running)?,
            json_text(&hb.interactive_use)?,
            hb.local_paused,
            hb.room.as_ref().map(json_text).transpose()?,
        ])?;
        fold_minute(&tx, hb, now)?;
        super::report::record_guard(
            &tx,
            id,
            &GuardState::of(&hb.guard),
            now,
            (offline_after_ms, max_spans),
        )?;
        super::quota::insert_snapshots(&tx, &hb.quota, now)?;
        tx.commit()?;
        Ok(previous)
    }

    /// Persists online to offline transitions and their events in one transaction.
    pub fn persist_offline_transitions(
        &mut self,
        now: UnixMs,
        offline_after_ms: i64,
    ) -> StoreResult<usize> {
        let tx = self.conn.transaction()?;
        let candidates = {
            let mut stmt =
                tx.prepare_cached("SELECT id, last_seen_ms, online FROM devices ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<(String, UnixMs, bool)>>>()?
        };
        let kind = enum_text(&EventKind::DeviceOffline)?;
        let mut changed = 0;
        {
            let mut mark =
                tx.prepare_cached("UPDATE devices SET online = 0 WHERE id = ?1 AND online = 1")?;
            let mut event = tx.prepare_cached(
                "INSERT INTO events (at_ms, kind, device, job, message)
                 VALUES (?1, ?2, ?3, NULL, ?4)",
            )?;
            for (id, last_seen_ms, online) in candidates {
                if offline_transition(online, last_seen_ms, now, offline_after_ms)
                    && mark.execute([&id])? == 1
                {
                    let silent_s = now.saturating_sub(last_seen_ms) / 1000;
                    let msg = format!("{id} offline: no heartbeat for {silent_s} s");
                    event.execute(params![now, kind, id, msg])?;
                    changed += 1;
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Every known device. Online means a heartbeat within `offline_after_ms` of `now`;
    /// paused means a Global or Device control is in effect.
    pub fn devices(&self, now: UnixMs, offline_after_ms: i64) -> StoreResult<Vec<DeviceView>> {
        let paused = self.paused_scopes()?;
        let global = paused.contains(&ControlScope::Global);
        let mut stmt = self.conn.prepare_cached(
            "SELECT info, last_seen_ms, metrics, guard, running, local_paused, room, dispatch_on,
                    agent_auth
             FROM devices ORDER BY id",
        )?;
        let rows = stmt.query_map([], device_from_row)?;
        rows.map(|row| {
            let mut view = row?;
            view.online = device_online(view.last_seen_ms, now, offline_after_ms);
            view.paused = global || paused.contains(&ControlScope::Device(view.info.id.clone()));
            Ok(view)
        })
        .collect()
    }

    /// Switches dispatcher work on the device on or off and logs the change. Returns the
    /// value before, or None for a device that never sent a heartbeat.
    pub fn set_device_dispatch(
        &mut self,
        id: &str,
        on: bool,
        now: UnixMs,
    ) -> StoreResult<Option<bool>> {
        let keep = self.caps.events;
        let tx = self.conn.transaction()?;
        let before: Option<bool> = tx
            .query_row("SELECT dispatch_on FROM devices WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if before.is_some_and(|b| b != on) {
            tx.execute(
                "UPDATE devices SET dispatch_on = ?2 WHERE id = ?1",
                params![id, on],
            )?;
            let state = if on { "on" } else { "off" };
            let msg = format!("device {id} switched {state} for dispatcher work");
            let device = id.to_string();
            let kind = EventKind::Control;
            super::events::insert_event(&tx, keep, kind, Some(&device), None, &msg, now)?;
        }
        tx.commit()?;
        Ok(before)
    }

    /// Newest interactive use per CLI name across all devices, sorted by name.
    pub fn interactive_use(&self) -> StoreResult<Vec<(String, UnixMs)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT interactive_use FROM devices")?;
        let lists = stmt.query_map([], |r| json_col::<Vec<(String, UnixMs)>>(r, 0))?;
        let mut newest: BTreeMap<String, UnixMs> = BTreeMap::new();
        for list in lists {
            for (cli, at) in list? {
                let slot = newest.entry(cli).or_insert(at);
                *slot = (*slot).max(at);
            }
        }
        Ok(newest.into_iter().collect())
    }
}

fn fold_minute(conn: &Connection, hb: &Heartbeat, now: UnixMs) -> StoreResult<()> {
    let m = &hb.metrics;
    let running = i64::try_from(hb.running.len()).unwrap_or(i64::MAX);
    conn.prepare_cached(FOLD_MINUTE)?.execute(params![
        hb.device.id,
        now - now.rem_euclid(MINUTE_MS),
        m.cpu_busy_pct,
        m.load_avg_1m,
        m.mem_free_pct,
        u64_to_sql(m.mem_available_bytes),
        u64_to_sql(m.swap_used_bytes),
        u64_to_sql(m.disk_free_bytes),
        m.user_idle_secs.map(u64_to_sql),
        m.on_ac_power,
        running,
        hb.guard.action.severity(),
    ])?;
    Ok(())
}

fn device_from_row(r: &Row<'_>) -> rusqlite::Result<DeviceView> {
    let stored: StoredDevice = json_col(r, 0)?;
    Ok(DeviceView {
        info: stored.info,
        keep_awake: stored.keep_awake,
        last_seen_ms: r.get(1)?,
        metrics: opt_json_col(r, 2)?,
        guard: opt_json_col(r, 3)?,
        running: json_col(r, 4)?,
        local_paused: r.get(5)?,
        online: false,
        paused: false,
        capacity: None,
        room: opt_json_col(r, 6)?,
        enabled: Some(r.get(7)?),
        agent_auth: r
            .get::<_, Option<String>>(8)?
            .map(|_| super::enum_col(r, 8))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use idlemax_core::protocol::Heartbeat;
    use rusqlite::Connection;

    use super::super::{schema, Store};

    const T0: i64 = 1_800_000_000_000;

    fn beat(s: &mut Store, at: i64) {
        let mut hb = Heartbeat::default();
        hb.device.id = "mac".into();
        s.record_heartbeat(&hb, at).unwrap();
    }

    fn enabled(s: &Store) -> Option<bool> {
        s.devices(T0, i64::MAX).unwrap()[0].enabled
    }

    #[test]
    fn a_device_starts_off_and_a_heartbeat_keeps_its_switch() {
        let mut s = Store::open_in_memory().unwrap();
        beat(&mut s, T0);
        assert_eq!(enabled(&s), Some(false));
        assert_eq!(s.set_device_dispatch("mac", true, T0).unwrap(), Some(false));
        assert_eq!(s.set_device_dispatch("mac", true, T0).unwrap(), Some(true));
        beat(&mut s, T0 + 1);
        assert_eq!(enabled(&s), Some(true));
        assert_eq!(s.set_device_dispatch("nope", true, T0).unwrap(), None);
    }

    #[test]
    fn a_device_from_before_the_switch_migrates_off() {
        let conn = Connection::open_in_memory().unwrap();
        let before = schema::MIGRATIONS.len() - 1;
        for sql in &schema::MIGRATIONS[..before] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before as i64)
            .unwrap();
        conn.execute(
            "INSERT INTO devices (id, info, first_seen_ms, last_seen_ms)
             VALUES ('mac', '{\"id\":\"mac\"}', ?1, ?1)",
            [T0],
        )
        .unwrap();
        let s = Store::init(conn, false).unwrap();
        assert_eq!(enabled(&s), Some(false));
    }
}
