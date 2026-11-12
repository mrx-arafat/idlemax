//! Device credentials (adapter): one SHA-256 digest per device, never the credential itself,
//! so a copy of the database opens no route (A15).

use idlemax_core::control::credential::AgentAuth;
use idlemax_core::types::{DeviceId, UnixMs};
use rusqlite::{params, OptionalExtension};

use super::{enum_text, Store, StoreResult};

impl Store {
    /// Stores the digest of a new credential for `device`, replacing an older one.
    pub fn set_device_credential(
        &mut self,
        device: &str,
        digest: &str,
        now: UnixMs,
    ) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO device_credentials (device, digest, created_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (device) DO UPDATE SET digest = excluded.digest,
                                                created_ms = excluded.created_ms",
            params![device, digest, now],
        )?;
        Ok(())
    }

    /// Removes the device's credential; false when it had none.
    pub fn delete_device_credential(&mut self, device: &str) -> StoreResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM device_credentials WHERE device = ?1", [device])?;
        Ok(n > 0)
    }

    /// The device a credential digest belongs to.
    pub fn credential_device(&self, digest: &str) -> StoreResult<Option<DeviceId>> {
        Ok(self
            .conn
            .query_row(
                "SELECT device FROM device_credentials WHERE digest = ?1",
                [digest],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records how the device's agent proved itself on this heartbeat.
    pub fn set_agent_auth(&mut self, device: &str, auth: AgentAuth) -> StoreResult<()> {
        self.conn.execute(
            "UPDATE devices SET agent_auth = ?2 WHERE id = ?1",
            params![device, enum_text(&auth)?],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::protocol::Heartbeat;

    use super::*;

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn a_new_credential_replaces_the_old_and_delete_removes_it() {
        let mut s = Store::open_in_memory().unwrap();
        s.set_device_credential("box", "d1", T0).unwrap();
        assert_eq!(s.credential_device("d1").unwrap().as_deref(), Some("box"));
        s.set_device_credential("box", "d2", T0).unwrap();
        assert_eq!(s.credential_device("d1").unwrap(), None);
        assert_eq!(s.credential_device("d2").unwrap().as_deref(), Some("box"));
        assert!(s.delete_device_credential("box").unwrap());
        assert!(!s.delete_device_credential("box").unwrap());
        assert_eq!(s.credential_device("d2").unwrap(), None);
    }

    #[test]
    fn the_agent_auth_of_the_last_heartbeat_is_served() {
        let mut s = Store::open_in_memory().unwrap();
        let mut hb = Heartbeat::default();
        hb.device.id = "box".into();
        s.record_heartbeat(&hb, T0).unwrap();
        assert_eq!(s.devices(T0, i64::MAX).unwrap()[0].agent_auth, None);
        s.set_agent_auth("box", AgentAuth::Credential).unwrap();
        let auth = s.devices(T0, i64::MAX).unwrap()[0].agent_auth;
        assert_eq!(auth, Some(AgentAuth::Credential));
    }
}
