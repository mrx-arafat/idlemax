//! Adapter: reads the Claude CLI login status via `claude auth status --json`.

use std::ffi::OsStr;
use std::time::Duration;

use idlemax_core::quota::login::{claude_login, Login};

use super::cmd;
use crate::doctor::facts::which_in;

/// Timeout for reading the claude auth status command.
pub const LOGIN_CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads the Claude login status by executing `claude auth status --json` found on `path`.
pub fn read_claude_login(path: &OsStr) -> Login {
    let Some(bin) = which_in("claude", path) else {
        return Login::Unknown {
            reason: "claude not found on PATH".to_string(),
        };
    };
    let Some(bin_str) = bin.to_str() else {
        return Login::Unknown {
            reason: "claude binary path is not valid UTF-8".to_string(),
        };
    };
    match cmd::run(bin_str, &["auth", "status", "--json"], LOGIN_CMD_TIMEOUT) {
        Some(stdout) => claude_login(&stdout),
        None => Login::Unknown {
            reason: "claude auth status --json timed out or failed to start".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_read_claude_login_with_stub() {
        let dir = tempdir().unwrap();
        let stub = dir.path().join("claude");
        let script = "#!/bin/sh\necho '{\"loggedIn\": true, \"orgId\": \"org-stub-test\"}'\n";
        fs::write(&stub, script).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        let login = read_claude_login(dir.path().as_os_str());
        assert_eq!(
            login,
            Login::In {
                id: Some("org-stub-test".to_string())
            }
        );
    }

    #[test]
    fn test_read_claude_login_missing() {
        let dir = tempdir().unwrap();
        let login = read_claude_login(dir.path().as_os_str());
        assert!(matches!(login, Login::Unknown { .. }));
    }
}
