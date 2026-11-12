//! Domain: the worker sandbox of A13: whether one launch is wrapped in the OS sandbox, runs
//! with the soft fences only, or is refused, and which network hosts its proxy opens. Pure.

pub mod bwrap;
pub mod claude_home;

use serde::{Deserialize, Serialize};

use super::WorkerCli;
use crate::doctor::Check;
use crate::types::DeviceProfile;

/// `workers.sandbox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxSetting {
    #[default]
    Auto,
    Required,
    Off,
}

/// What the probe of the OS sandbox found on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Usable,
    /// Why it cannot be used, such as "bwrap is not installed".
    Unusable(String),
}

/// How one launch runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxUse {
    Wrap,
    Bare(String),
    Refuse(String),
}

/// Wraps the launch when the sandbox is usable; else `auto` runs it with the soft fences and
/// a note, and `required` refuses it. A production device reads `auto` as `required`.
pub fn decide(
    setting: SandboxSetting,
    profile: Option<DeviceProfile>,
    probe: &Probe,
) -> SandboxUse {
    let setting = match (setting, profile) {
        (SandboxSetting::Auto, Some(DeviceProfile::Production)) => SandboxSetting::Required,
        (s, _) => s,
    };
    match (setting, probe) {
        (SandboxSetting::Off, _) => SandboxUse::Bare("the sandbox is off (workers.sandbox)".into()),
        (_, Probe::Usable) => SandboxUse::Wrap,
        (SandboxSetting::Auto, Probe::Unusable(why)) => {
            SandboxUse::Bare(format!("no sandbox, soft fences only: {why}"))
        }
        (_, Probe::Unusable(why)) => SandboxUse::Refuse(format!(
            "the worker sandbox is required and cannot be used: {why}; install bubblewrap, \
             or set workers.sandbox to auto"
        )),
    }
}

/// The doctor check: workers run sandboxed, run with the soft fences only (`warn`), or cannot
/// start because the sandbox is required (`fail`); `off` is the owner's choice and `ok`. With
/// no worker CLI configured (`workers.clis`) no worker starts here, so it only informs.
pub fn doctor_check(
    setting: SandboxSetting,
    profile: Option<DeviceProfile>,
    probe: &Probe,
    workers_on: bool,
) -> Check {
    const ID: &str = "sandbox";
    if !workers_on {
        let state = match probe {
            Probe::Usable => "bubblewrap is usable".to_string(),
            Probe::Unusable(why) => why.clone(),
        };
        return Check::ok(
            ID,
            format!("no worker CLI is configured (workers.clis); {state}"),
        );
    }
    const FIX: &str =
        "install bubblewrap (sudo apt install bubblewrap), then run idlemax doctor again";
    match decide(setting, profile, probe) {
        SandboxUse::Wrap => Check::ok(
            ID,
            "workers run inside bubblewrap with the allow-list proxy",
        ),
        SandboxUse::Bare(_) if setting == SandboxSetting::Off => Check::ok(
            ID,
            "off (workers.sandbox): workers run with the soft fences only",
        ),
        SandboxUse::Bare(note) => Check::warn(ID, format!("workers run with {note}"), FIX),
        SandboxUse::Refuse(why) => Check::fail(ID, format!("no worker can start: {why}"), FIX),
    }
}

/// The model API hosts a worker of `cli` reaches when `workers.sandbox_hosts` names none.
pub fn default_hosts(cli: WorkerCli) -> Vec<String> {
    let hosts: &[&str] = match cli {
        WorkerCli::Claude => &[
            "api.anthropic.com",
            "console.anthropic.com",
            "claude.ai",
            "statsig.anthropic.com",
        ],
        WorkerCli::Codex => &["api.openai.com", "chatgpt.com", "auth.openai.com"],
        WorkerCli::Opencode | WorkerCli::Agy => &[],
    };
    hosts.iter().map(|h| h.to_string()).collect()
}

/// `host:port` of a `CONNECT` is allowed only on port 443 of a host on the list, exactly.
pub fn host_allowed(hosts: &[String], target: &str) -> bool {
    let Some((host, port)) = target.rsplit_once(':') else {
        return false;
    };
    port == "443" && hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_BWRAP: &str = "bwrap is not installed";

    #[test]
    fn a_usable_sandbox_wraps_and_a_missing_one_runs_bare_only_in_auto() {
        let gone = Probe::Unusable(NO_BWRAP.into());
        let laptop = Some(DeviceProfile::Laptop);
        assert_eq!(
            decide(SandboxSetting::Auto, laptop, &Probe::Usable),
            SandboxUse::Wrap
        );
        assert_eq!(
            decide(SandboxSetting::Required, laptop, &Probe::Usable),
            SandboxUse::Wrap
        );
        assert!(
            matches!(decide(SandboxSetting::Auto, laptop, &gone), SandboxUse::Bare(n) if n.contains(NO_BWRAP))
        );
        assert!(
            matches!(decide(SandboxSetting::Required, laptop, &gone), SandboxUse::Refuse(n) if n.contains(NO_BWRAP))
        );
        assert!(matches!(
            decide(SandboxSetting::Auto, Some(DeviceProfile::Production), &gone),
            SandboxUse::Refuse(n) if n.contains(NO_BWRAP)
        ));
        assert!(
            matches!(decide(SandboxSetting::Off, laptop, &Probe::Usable), SandboxUse::Bare(n) if n.contains("workers.sandbox"))
        );
    }

    #[test]
    fn doctor_says_ok_warn_or_fail_for_the_sandbox() {
        use crate::doctor::Level;
        let gone = Probe::Unusable(NO_BWRAP.into());
        let level = |s, p, probe| doctor_check(s, p, probe, true).level;
        assert_eq!(level(SandboxSetting::Auto, None, &Probe::Usable), Level::Ok);
        let warn = doctor_check(SandboxSetting::Auto, None, &gone, true);
        assert_eq!(warn.level, Level::Warn);
        assert!(warn.detail.contains(NO_BWRAP), "{}", warn.detail);
        assert_eq!(level(SandboxSetting::Required, None, &gone), Level::Fail);
        let production = Some(DeviceProfile::Production);
        assert_eq!(level(SandboxSetting::Auto, production, &gone), Level::Fail);
        assert_eq!(level(SandboxSetting::Off, None, &gone), Level::Ok);
        let idle = doctor_check(SandboxSetting::Required, production, &gone, false);
        assert_eq!(
            idle.level,
            Level::Ok,
            "no worker CLI: nothing can be refused"
        );
    }

    #[test]
    fn only_port_443_of_an_exact_host_is_allowed() {
        let hosts = default_hosts(WorkerCli::Claude);
        assert!(host_allowed(&hosts, "api.anthropic.com:443"));
        assert!(host_allowed(&hosts, "API.Anthropic.com:443"));
        assert!(!host_allowed(&hosts, "api.anthropic.com:80"));
        assert!(!host_allowed(&hosts, "evil.api.anthropic.com:443"));
        assert!(!host_allowed(&hosts, "api.anthropic.com.evil.io:443"));
        assert!(!host_allowed(&hosts, "api.anthropic.com"));
    }
}
