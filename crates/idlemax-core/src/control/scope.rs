//! Domain: refuse a pause or stop-all that names a device or account nobody has, so a typo
//! never reports success while the real one keeps running. Pure.

use crate::protocol::ControlRequest;
use crate::types::{ControlAction, ControlScope};

/// Why `req` names a device or account that does not exist, listing the known names. A
/// resume is never refused, so a flag left behind under any name can always be removed.
pub fn unknown_scope(
    req: &ControlRequest,
    devices: &[String],
    accounts: &[String],
) -> Option<String> {
    if req.action == ControlAction::Resume {
        return None;
    }
    let (kind, name, known) = match &req.scope {
        ControlScope::Device(d) => ("device", d, devices),
        ControlScope::Account(a) => ("account", a, accounts),
        _ => return None,
    };
    if known.iter().any(|k| k == name) {
        return None;
    }
    Some(if known.is_empty() {
        format!("unknown {kind} `{name}`: there are no {kind}s yet")
    } else {
        format!(
            "unknown {kind} `{name}`: known {kind}s are {}",
            known.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::unknown_scope;
    use crate::protocol::ControlRequest;
    use crate::types::{ControlAction, ControlScope};

    fn req(action: ControlAction, scope: ControlScope) -> ControlRequest {
        ControlRequest {
            action,
            scope,
            reason: None,
        }
    }

    #[test]
    fn a_pause_naming_a_device_or_account_nobody_has_is_refused_but_resume_never_is() {
        let devices = vec!["laptop".to_string(), "server".to_string()];
        let accounts = vec!["claude".to_string()];
        let check = |action, scope| unknown_scope(&req(action, scope), &devices, &accounts);
        assert_eq!(
            check(ControlAction::Pause, ControlScope::Device("lapto".into())).as_deref(),
            Some("unknown device `lapto`: known devices are laptop, server")
        );
        assert_eq!(
            check(
                ControlAction::StopAll,
                ControlScope::Account("claud".into())
            )
            .as_deref(),
            Some("unknown account `claud`: known accounts are claude")
        );
        assert_eq!(
            check(ControlAction::Pause, ControlScope::Device("laptop".into())),
            None
        );
        assert_eq!(
            check(ControlAction::Resume, ControlScope::Device("gone".into())),
            None
        );
        assert_eq!(check(ControlAction::Pause, ControlScope::Global), None);
        assert_eq!(
            unknown_scope(
                &req(ControlAction::Pause, ControlScope::Account("x".into())),
                &[],
                &[]
            )
            .as_deref(),
            Some("unknown account `x`: there are no accounts yet")
        );
    }
}
