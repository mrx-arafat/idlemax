//! Newest advisor inputs per device (adapter): one row each, replaced on every send.

use idlemax_core::advice::{AdvisorInputs, StoredInputs};
use idlemax_core::types::UnixMs;
use rusqlite::params;

use super::{json_col, json_text, Store, StoreError, StoreResult, HISTORY_RETENTION_MS};

/// Largest advisor-inputs document kept per device (a real one is a few KiB).
const MAX_INPUT_BYTES: usize = 256 * 1024;

impl Store {
    /// Replaces the device's copy and drops copies of devices silent for 30 days.
    pub fn put_advisor_inputs(&mut self, inputs: &AdvisorInputs, now: UnixMs) -> StoreResult<()> {
        if inputs.device.trim().is_empty() {
            return Err(StoreError::Invalid("advisor inputs device is empty".into()));
        }
        let payload = json_text(inputs)?;
        if payload.len() > MAX_INPUT_BYTES {
            return Err(StoreError::Invalid(format!(
                "advisor inputs are over {MAX_INPUT_BYTES} bytes"
            )));
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO advisor_inputs (device, received_ms, payload) VALUES (?1, ?2, ?3)
             ON CONFLICT (device) DO UPDATE SET
               received_ms = excluded.received_ms, payload = excluded.payload",
            params![inputs.device, now, payload],
        )?;
        tx.execute(
            "DELETE FROM advisor_inputs WHERE received_ms < ?1",
            [now.saturating_sub(HISTORY_RETENTION_MS)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every device's newest copy.
    pub fn advisor_inputs(&self) -> StoreResult<Vec<StoredInputs>> {
        let mut stmt = self
            .conn
            .prepare("SELECT received_ms, payload FROM advisor_inputs ORDER BY device")?;
        let rows = stmt.query_map([], |r| {
            Ok(StoredInputs {
                received_ms: r.get(0)?,
                inputs: json_col(r, 1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
