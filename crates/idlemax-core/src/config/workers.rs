//! Domain: the `[workers]` section of `config.toml`: which agent CLIs may run workers and
//! review, the review cap and the time limits of a worker launch. Pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::worker::sandbox::SandboxSetting;

/// The CLIs a worker or reviewer may use for now (A21).
const KNOWN_CLIS: [&str; 2] = ["claude", "codex"];

/// Which host a worker launch runs in: Herdr pane or headless process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerHostChoice {
    #[default]
    Auto,
    Herdr,
    Headless,
}

impl WorkerHostChoice {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Herdr => "herdr",
            Self::Headless => "headless",
        }
    }
}

/// Worker settings. The numbers are starting values (A25).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkersConfig {
    /// Agent CLIs that may run workers; empty until the proof of Task 12 passed for one.
    pub clis: Vec<String>,
    /// Agent CLIs that may review a high-risk diff.
    pub reviewers: Vec<String>,
    /// Which host a worker launch uses (`auto`, `herdr`, `headless`).
    pub host: WorkerHostChoice,
    /// Whether workers run inside the OS sandbox (`auto`, `required`, `off`; A13).
    pub sandbox: SandboxSetting,
    /// The hosts a sandboxed worker of each CLI reaches; a CLI not listed gets the defaults.
    pub sandbox_hosts: BTreeMap<String, Vec<String>>,
    /// A file only the owner reads (mode 0600) holding a long-lived Claude token from
    /// `claude setup-token`; a sandboxed Claude worker signs in with it, and the owner's
    /// `~/.claude` is not bound at all (review finding 6).
    pub claude_token_file: Option<String>,
    /// Finished tasks per project that may wait for the owner's review (D9).
    pub review_cap: u32,
    /// Seconds between two launches on one device (D13).
    pub settle_secs: u64,
    /// Awake seconds one worker launch may run.
    pub max_runtime_secs: u64,
    /// Seconds a worker may wait for input before it is stopped.
    pub blocked_secs: u64,
    /// A diff of more added plus removed lines is high risk (D8).
    pub review_diff_lines: u64,
    pub setup_secs: u64,
    pub check_secs: u64,
}

impl Default for WorkersConfig {
    fn default() -> Self {
        Self {
            clis: Vec::new(),
            reviewers: Vec::new(),
            host: WorkerHostChoice::Auto,
            sandbox: SandboxSetting::Auto,
            sandbox_hosts: BTreeMap::new(),
            claude_token_file: None,
            review_cap: 3,
            settle_secs: 60,
            max_runtime_secs: 3600,
            blocked_secs: 120,
            review_diff_lines: 300,
            setup_secs: 900,
            check_secs: 600,
        }
    }
}

impl WorkersConfig {
    /// Refuses a cap or a time of 0 and an unknown CLI. The error starts with the key.
    pub fn validate(&self) -> Result<(), String> {
        if self.review_cap == 0 {
            return Err("workers.review_cap: must be above 0".into());
        }
        let times = [
            ("settle_secs", self.settle_secs),
            ("max_runtime_secs", self.max_runtime_secs),
            ("blocked_secs", self.blocked_secs),
            ("review_diff_lines", self.review_diff_lines),
            ("setup_secs", self.setup_secs),
            ("check_secs", self.check_secs),
        ];
        for (key, v) in times {
            if v == 0 {
                return Err(format!("workers.{key}: must be above 0"));
            }
        }
        if let Some(file) = &self.claude_token_file {
            if !std::path::Path::new(file).is_absolute() {
                return Err(format!(
                    "workers.claude_token_file: {file:?} is not an absolute path"
                ));
            }
        }
        for (key, list) in [("clis", &self.clis), ("reviewers", &self.reviewers)] {
            if let Some(bad) = list.iter().find(|c| !KNOWN_CLIS.contains(&c.as_str())) {
                return Err(format!(
                    "workers.{key}: {bad:?} is not one of {}",
                    KNOWN_CLIS.join(", ")
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{plan_reload, Config};

    #[test]
    fn a_zero_review_cap_is_refused_by_its_key() {
        let err = Config::parse("[workers]\nreview_cap = 0\n").unwrap_err();
        assert!(err.to_string().starts_with("workers.review_cap"), "{err}");
    }

    #[test]
    fn an_unknown_cli_is_refused() {
        let err = Config::parse("[workers]\nclis = [\"agy\"]\n").unwrap_err();
        assert!(err.to_string().starts_with("workers.clis"), "{err}");
    }

    #[test]
    fn an_empty_file_has_no_worker_clis_and_a_cap_of_three() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.workers.clis.is_empty());
        assert_eq!(cfg.workers.review_cap, 3);
    }

    #[test]
    fn a_changed_worker_key_applies_without_a_restart() {
        let old = Config::parse("").unwrap();
        let new = Config::parse("[workers]\nreview_cap = 2\n").unwrap();
        let report = plan_reload(&old, &new).unwrap();
        assert_eq!(report.applied, ["workers.review_cap"]);
    }

    #[test]
    fn herdr_host_is_refused_on_production_profile() {
        let err = Config::parse("[agent]\nprofile = \"production\"\n[workers]\nhost = \"herdr\"\n")
            .unwrap_err();
        assert!(err.to_string().starts_with("workers.host"), "{err}");
    }

    #[test]
    fn sandbox_off_is_refused_on_production_profile() {
        let err =
            Config::parse("[agent]\nprofile = \"production\"\n[workers]\nsandbox = \"off\"\n")
                .unwrap_err();
        assert!(err.to_string().starts_with("workers.sandbox"), "{err}");
        let cfg = Config::parse("[workers]\nsandbox = \"required\"\n").unwrap();
        assert_eq!(
            cfg.workers.sandbox,
            crate::worker::sandbox::SandboxSetting::Required
        );
    }

    #[test]
    fn the_worker_token_file_is_an_absolute_path() {
        let cfg =
            Config::parse("[workers]\nclaude_token_file = \"/h/.idlemax-worker-token\"\n").unwrap();
        assert_eq!(
            cfg.workers.claude_token_file.as_deref(),
            Some("/h/.idlemax-worker-token")
        );
        let err = Config::parse("[workers]\nclaude_token_file = \"token\"\n").unwrap_err();
        assert!(
            err.to_string().starts_with("workers.claude_token_file"),
            "{err}"
        );
    }

    #[test]
    fn workers_host_key_applies_without_restart() {
        let old = Config::parse("").unwrap();
        let new = Config::parse("[workers]\nhost = \"headless\"\n").unwrap();
        let report = plan_reload(&old, &new).unwrap();
        assert_eq!(report.applied, ["workers.host"]);
    }
}
