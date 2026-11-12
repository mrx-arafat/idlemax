//! Domain: who a bearer on the coordinator speaks for (A15). The owner token opens every
//! route; a device credential opens only the agent routes and speaks only for its device.
//! Pure: the adapter matches the bearer and passes the result in.

use serde::{Deserialize, Serialize};

use crate::types::DeviceId;

/// The routes a device credential opens.
const AGENT_ROUTES: &str = "/v1/agent/";

/// Who presented the bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Owner,
    Device(DeviceId),
}

/// How a device's agent proved itself on its last heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAuth {
    Credential,
    OwnerTokenLocal,
    OwnerTokenRemote,
}

/// A device credential opens `/v1/agent/*` and nothing else.
pub fn route_allowed(identity: &Identity, path: &str) -> bool {
    match identity {
        Identity::Owner => true,
        Identity::Device(_) => path.starts_with(AGENT_ROUTES),
    }
}

/// A device credential speaks for its own device only.
pub fn speaks_for(identity: &Identity, claimed: &str) -> Result<(), String> {
    match identity {
        Identity::Device(own) if own != claimed => Err(format!(
            "this credential belongs to device {own} and cannot speak for device {claimed}"
        )),
        _ => Ok(()),
    }
}

/// A heartbeat with the owner token is local when its peer is the coordinator's own host.
pub fn agent_auth(identity: &Identity, same_host: bool) -> AgentAuth {
    match (identity, same_host) {
        (Identity::Device(_), _) => AgentAuth::Credential,
        (Identity::Owner, true) => AgentAuth::OwnerTokenLocal,
        (Identity::Owner, false) => AgentAuth::OwnerTokenRemote,
    }
}

/// `POST /v1/devices/{id}/credential`: the credential, shown this once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialReply {
    pub device: DeviceId,
    pub credential: String,
}

/// `DELETE /v1/devices/{id}/credential`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeReply {
    pub device: DeviceId,
    pub removed: bool,
}

/// The words of the 403 a device credential gets, which `idlemax doctor` recognizes.
pub const ONLY_AGENT_ROUTES: &str = "opens only the agent routes";

/// The 403 text for a device credential on a route it does not open.
pub fn refused(device: &str) -> String {
    format!(
        "this credential belongs to device {device} and {ONLY_AGENT_ROUTES}; \
         run owner commands from a device that holds the owner token"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str) -> Identity {
        Identity::Device(id.into())
    }

    #[test]
    fn a_device_credential_opens_only_the_agent_routes() {
        for path in ["/v1/status", "/v1/devices/x/on", "/", "/v1/agent/heartbeat"] {
            assert!(route_allowed(&Identity::Owner, path), "{path}");
        }
        assert!(route_allowed(&device("box"), "/v1/agent/heartbeat"));
        for path in ["/v1/status", "/v1/devices/x/on", "/", "/v1/agentx"] {
            assert!(!route_allowed(&device("box"), path), "{path}");
        }
    }

    #[test]
    fn a_device_credential_speaks_for_its_own_device_only() {
        assert!(speaks_for(&Identity::Owner, "any").is_ok());
        assert!(speaks_for(&device("box"), "box").is_ok());
        let err = speaks_for(&device("box"), "mac").unwrap_err();
        assert!(err.contains("box") && err.contains("mac"), "{err}");
    }

    #[test]
    fn the_agent_auth_says_how_a_heartbeat_proved_itself() {
        assert_eq!(agent_auth(&device("box"), false), AgentAuth::Credential);
        assert_eq!(
            agent_auth(&Identity::Owner, true),
            AgentAuth::OwnerTokenLocal
        );
        assert_eq!(
            agent_auth(&Identity::Owner, false),
            AgentAuth::OwnerTokenRemote
        );
    }
}
