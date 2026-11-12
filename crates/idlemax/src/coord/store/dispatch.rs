//! Dispatcher rows (adapter): the newest dispatch inputs per device. Base checks, pass rows and
//! task judgments join here in later steps. The domain decides; this file persists.

use idlemax_core::dispatch::inputs::DispatchInputs;
use idlemax_core::types::UnixMs;
use rusqlite::params;

use super::{json_text, Store, StoreError, StoreResult, HISTORY_RETENTION_MS};

/// Largest dispatch-inputs document kept per device (a real one is a few KiB).
const MAX_INPUT_BYTES: usize = 256 * 1024;

impl Store {
    /// Replaces the device's copy and drops copies of devices silent for 30 days.
    pub fn put_dispatch_inputs(&mut self, inputs: &DispatchInputs, now: UnixMs) -> StoreResult<()> {
        if inputs.device.trim().is_empty() {
            return Err(StoreError::Invalid(
                "dispatch inputs device is empty".into(),
            ));
        }
        let payload = json_text(inputs)?;
        if payload.len() > MAX_INPUT_BYTES {
            return Err(StoreError::Invalid(format!(
                "dispatch inputs are over {MAX_INPUT_BYTES} bytes"
            )));
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO dispatch_inputs (device, received_ms, payload) VALUES (?1, ?2, ?3)
             ON CONFLICT (device) DO UPDATE SET
               received_ms = excluded.received_ms, payload = excluded.payload",
            params![inputs.device, now, payload],
        )?;
        tx.execute(
            "DELETE FROM dispatch_inputs WHERE received_ms < ?1",
            [now.saturating_sub(HISTORY_RETENTION_MS)],
        )?;
        tx.commit()?;
        Ok(())
    }
}
