//! Domain: coordinator control of per-device dispatcher switches.
//! Pure: no I/O, no clock.

use serde::{Deserialize, Serialize};

use super::ControlError;
use crate::protocol::DeviceView;

/// Outcome of planning a device switch change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Switch {
    Changed,
    Already,
}

impl Switch {
    pub fn changed(self) -> bool {
        matches!(self, Switch::Changed)
    }
}

/// What changing a device's dispatcher switch does: refuses an unknown device id,
/// notes when the switch already had that value, or accepts the change.
pub fn plan_switch(
    known: &[String],
    current: Option<bool>,
    id: &str,
    on: bool,
) -> Result<Switch, ControlError> {
    let Some(cur) = current.filter(|_| known.iter().any(|k| k == id)) else {
        let msg = if known.is_empty() {
            format!("unknown device `{id}`: there are no devices yet")
        } else {
            format!(
                "unknown device `{id}`: known devices are {}",
                known.join(", ")
            )
        };
        return Err(ControlError::UnknownDevice(msg));
    };
    if cur == on {
        Ok(Switch::Already)
    } else {
        Ok(Switch::Changed)
    }
}

/// `device <id> is off: idlemax device on <id>`
pub fn off_reason(id: &str) -> String {
    format!("device {id} is off: idlemax device on {id}")
}

/// Why this device takes no dispatcher work whatever its room, or None.
pub fn device_gate(view: &DeviceView) -> Option<String> {
    if view.enabled == Some(true) {
        None
    } else {
        Some(off_reason(&view.info.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DeviceView;
    use crate::types::DeviceInfo;

    fn view(id: &str, enabled: Option<bool>) -> DeviceView {
        DeviceView {
            info: DeviceInfo {
                id: id.into(),
                ..Default::default()
            },
            enabled,
            ..Default::default()
        }
    }

    #[test]
    fn off_reason_formats_expected_command() {
        assert_eq!(
            off_reason("mac"),
            "device mac is off: idlemax device on mac"
        );
        assert_eq!(
            off_reason("linux-server"),
            "device linux-server is off: idlemax device on linux-server"
        );
    }

    #[test]
    fn device_gate_allows_only_explicitly_enabled_devices() {
        assert_eq!(device_gate(&view("mac", Some(true))), None);
        assert_eq!(
            device_gate(&view("mac", Some(false))),
            Some("device mac is off: idlemax device on mac".into())
        );
        assert_eq!(
            device_gate(&view("mac", None)),
            Some("device mac is off: idlemax device on mac".into())
        );
    }

    #[test]
    fn plan_switch_refuses_unknown_device_and_lists_known_devices() {
        let known = vec!["mac".to_string(), "box".to_string()];
        let err = plan_switch(&known, None, "nope", true).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown device `nope`: known devices are mac, box"
        );

        let err_empty = plan_switch(&[], None, "nope", true).unwrap_err();
        assert_eq!(
            err_empty.to_string(),
            "unknown device `nope`: there are no devices yet"
        );

        // If known does not have id, even if current is Some, it is unknown
        let err_mismatch = plan_switch(&known, Some(false), "other", true).unwrap_err();
        assert_eq!(
            err_mismatch.to_string(),
            "unknown device `other`: known devices are mac, box"
        );
    }

    #[test]
    fn plan_switch_detects_changes_and_noops() {
        let known = vec!["mac".to_string()];

        // Off to on -> Changed
        assert_eq!(
            plan_switch(&known, Some(false), "mac", true).unwrap(),
            Switch::Changed
        );
        // On to on -> Already
        assert_eq!(
            plan_switch(&known, Some(true), "mac", true).unwrap(),
            Switch::Already
        );
        // On to off -> Changed
        assert_eq!(
            plan_switch(&known, Some(true), "mac", false).unwrap(),
            Switch::Changed
        );
        // Off to off -> Already
        assert_eq!(
            plan_switch(&known, Some(false), "mac", false).unwrap(),
            Switch::Already
        );

        assert!(Switch::Changed.changed());
        assert!(!Switch::Already.changed());
    }
}
