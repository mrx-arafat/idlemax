//! Domain: AI quota login status parsing and preservation (pure; time is a parameter).

use serde::{Deserialize, Serialize};

use crate::types::UnixMs;

pub const LOGIN_KEEP_MS: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Login {
    In {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Out,
    Unknown {
        reason: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStatus {
    logged_in: bool,
    #[serde(default)]
    org_id: Option<String>,
}

pub fn claude_login(stdout: &str) -> Login {
    match serde_json::from_str::<ClaudeStatus>(stdout) {
        Ok(status) => {
            if status.logged_in {
                Login::In {
                    id: status.org_id.filter(|s| !s.trim().is_empty()),
                }
            } else {
                Login::Out
            }
        }
        Err(e) => Login::Unknown {
            reason: e.to_string(),
        },
    }
}

pub fn codex_login(exit_code: Option<i32>, stdout: &str) -> Login {
    match exit_code {
        Some(0) => Login::In { id: None },
        Some(1)
            if stdout.trim() == "Not logged in" || stdout.trim().starts_with("Not logged in") =>
        {
            Login::Out
        }
        Some(code) => Login::Unknown {
            reason: format!("codex login status exited with code {code}"),
        },
        None => Login::Unknown {
            reason: "codex login status timed out or was terminated".to_string(),
        },
    }
}

pub fn keep_last_good(last_good: Option<(&Login, UnixMs)>, new: Login, now_ms: UnixMs) -> Login {
    match new {
        Login::In { .. } | Login::Out => new,
        Login::Unknown { .. } => match last_good {
            Some((good @ Login::In { .. }, at_ms))
                if now_ms.saturating_sub(at_ms) <= LOGIN_KEEP_MS =>
            {
                good.clone()
            }
            _ => new,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_login_logged_in_with_org_id() {
        let fixture = r#"{
            "loggedIn": true,
            "orgId": "org-fake-12345",
            "email": "user@example.com",
            "subscriptionType": "pro"
        }"#;
        let res = claude_login(fixture);
        assert_eq!(
            res,
            Login::In {
                id: Some("org-fake-12345".to_string())
            }
        );
    }

    #[test]
    fn test_claude_login_logged_in_without_org_id() {
        let fixture = r#"{"loggedIn": true}"#;
        let res = claude_login(fixture);
        assert_eq!(res, Login::In { id: None });
    }

    #[test]
    fn test_claude_login_logged_out() {
        let fixture = r#"{"loggedIn": false}"#;
        let res = claude_login(fixture);
        assert_eq!(res, Login::Out);
    }

    #[test]
    fn test_claude_login_not_json() {
        let fixture = "something went wrong or 404 not found";
        let res = claude_login(fixture);
        assert!(matches!(res, Login::Unknown { .. }));
    }

    #[test]
    fn test_claude_login_drops_email_from_debug() {
        let fixture = r#"{
            "loggedIn": true,
            "orgId": "org-safe-id",
            "email": "secret-never-print@example.com"
        }"#;
        let res = claude_login(fixture);
        let debug = format!("{res:?}");
        assert!(
            !debug.contains("secret-never-print@example.com"),
            "debug output must not contain email: {debug}"
        );
    }

    #[test]
    fn test_codex_login_status() {
        assert_eq!(codex_login(Some(0), "Logged in"), Login::In { id: None });
        assert_eq!(codex_login(Some(1), "Not logged in\n"), Login::Out);
        assert!(matches!(
            codex_login(Some(2), "error"),
            Login::Unknown { .. }
        ));
        assert!(matches!(codex_login(None, ""), Login::Unknown { .. }));
    }

    #[test]
    fn test_keep_last_good() {
        let earlier_in = Login::In {
            id: Some("org-old".to_string()),
        };
        let t0: UnixMs = 1_000_000_000;
        let t_23h = t0 + 23 * 3_600_000;
        let t_25h = t0 + 25 * 3_600_000;

        let unknown = Login::Unknown {
            reason: "timed out".to_string(),
        };

        // 23 hours later: returns earlier In
        let r23 = keep_last_good(Some((&earlier_in, t0)), unknown.clone(), t_23h);
        assert_eq!(r23, earlier_in);

        // 25 hours later: returns Unknown
        let r25 = keep_last_good(Some((&earlier_in, t0)), unknown, t_25h);
        assert!(matches!(r25, Login::Unknown { .. }));

        // In or Out: always returns new
        let new_in = Login::In {
            id: Some("org-new".to_string()),
        };
        assert_eq!(
            keep_last_good(Some((&earlier_in, t0)), new_in.clone(), t_23h),
            new_in
        );
        assert_eq!(
            keep_last_good(Some((&earlier_in, t0)), Login::Out, t_23h),
            Login::Out
        );
    }
}
