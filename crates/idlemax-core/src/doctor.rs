//! Domain: the `idlemax doctor` report and the pure judgments behind each check.
//! Adapters gather the facts; nothing here touches the disk, the network or the clock.

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigError};
use crate::types::{QuotaSnapshot, UnixMs};

/// One check's outcome. A setup is healthy when no check fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn word(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One line of the report: what was checked, how it went, and the exact next step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub level: Level,
    pub detail: String,
    /// What to do about a warn or a fail; always present then.
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level: Level::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn warn(id: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level: Level::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    pub fn fail(id: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level: Level::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub ok: usize,
    pub warn: usize,
    pub fail: usize,
}

/// The whole run. `healthy` is false when any check failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub version: String,
    pub home: Option<String>,
    pub counts: Counts,
    pub healthy: bool,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn new(version: impl Into<String>, home: Option<String>) -> Self {
        Self {
            version: version.into(),
            home,
            counts: Counts::default(),
            healthy: true,
            checks: Vec::new(),
        }
    }

    pub fn set_home(&mut self, home: impl Into<String>) {
        self.home = Some(home.into());
    }

    pub fn push(&mut self, check: Check) {
        match check.level {
            Level::Ok => self.counts.ok += 1,
            Level::Warn => self.counts.warn += 1,
            Level::Fail => {
                self.counts.fail += 1;
                self.healthy = false;
            }
        }
        self.checks.push(check);
    }
}

/// Disk free against the guard's pause and kill lines. `None` when neither is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskVerdict {
    Ok,
    /// Below the pause line: no job will ever start.
    BelowPause {
        free_mib: u64,
        line_mib: u64,
    },
    /// Below the kill line: a running job gets killed.
    BelowKill {
        free_mib: u64,
        line_mib: u64,
    },
}

pub fn disk_verdict(
    free_mib: u64,
    pause_mib: Option<u64>,
    kill_mib: Option<u64>,
) -> Option<DiskVerdict> {
    match (pause_mib, kill_mib) {
        (None, None) => None,
        _ => {
            if let Some(line) = kill_mib.filter(|line| free_mib < *line) {
                return Some(DiskVerdict::BelowKill {
                    free_mib,
                    line_mib: line,
                });
            }
            if let Some(line) = pause_mib.filter(|line| free_mib < *line) {
                return Some(DiskVerdict::BelowPause {
                    free_mib,
                    line_mib: line,
                });
            }
            Some(DiskVerdict::Ok)
        }
    }
}

/// How trustworthy a quota probe's data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    Fresh { age_ms: i64 },
    Stale { age_ms: i64 },
    NoData,
}

pub fn probe_verdict(snaps: &[QuotaSnapshot], now_ms: UnixMs, max_age_ms: i64) -> ProbeVerdict {
    match snaps.iter().map(|s| s.at_ms).max() {
        None => ProbeVerdict::NoData,
        Some(at) => {
            let age_ms = (now_ms - at).max(0);
            if age_ms > max_age_ms {
                ProbeVerdict::Stale { age_ms }
            } else {
                ProbeVerdict::Fresh { age_ms }
            }
        }
    }
}

/// A CLI and coordinator built from different releases can disagree on the API.
pub fn version_mismatch(local: &str, remote: &str) -> bool {
    local != remote
}

/// "512 MiB", "15.0 GiB".
pub fn format_mib(mib: u64) -> String {
    if mib >= 1024 {
        format!("{:.1} GiB", mib as f64 / 1024.0)
    } else {
        format!("{mib} MiB")
    }
}

/// Keys in `text` that `Config` does not define (`[coordinatorr]`, `agent.sample_sec`).
/// Unknown keys are silently ignored by the parser, so a typo otherwise changes nothing.
///
/// The accepted key tree is read back from the parsed config itself: whatever the
/// parser kept is known, and a `None` field cannot be expressed in TOML, so a
/// round-trip never invents a false positive. `[guard]` needs no special case:
/// it is a free table whose own parser rejects unknown keys.
pub fn unknown_config_keys(text: &str) -> Result<Vec<String>, ConfigError> {
    let user: toml::Table = toml::from_str(text)?;
    let round_trip = Config::parse(text)?.to_toml_string()?;
    let known: toml::Table = toml::from_str(&round_trip)?;
    let mut out = Vec::new();
    diff(&user, &known, "", &mut out);
    out.sort();
    Ok(out)
}

/// Keys the user wrote that the parsed config does not hold.
fn diff(user: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in user {
        let path = match prefix.is_empty() {
            true => key.clone(),
            false => format!("{prefix}.{key}"),
        };
        match (value, known.get(key)) {
            (_, None) => out.push(path),
            (toml::Value::Table(user), Some(toml::Value::Table(known))) => {
                diff(user, known, &path, out)
            }
            (toml::Value::Array(user), Some(toml::Value::Array(known))) => {
                for (user, known) in user.iter().zip(known) {
                    if let (toml::Value::Table(user), toml::Value::Table(known)) = (user, known) {
                        diff(user, known, &path, out);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProbeSource, QuotaWindowKind};

    fn snap(account: &str, at_ms: i64) -> QuotaSnapshot {
        QuotaSnapshot {
            account: account.into(),
            window: QuotaWindowKind::Weekly,
            used_pct: 10.0,
            resets_at_ms: None,
            source: ProbeSource::Manual,
            at_ms,
        }
    }

    #[test]
    fn report_counts_turn_healthy_off_on_the_first_fail() {
        let mut r = Report::new("1.2.3", None);
        assert!(r.healthy);
        r.push(Check::ok("a", "fine"));
        r.push(Check::warn("b", "meh", "do x"));
        assert!(r.healthy);
        r.push(Check::fail("c", "broken", "do y"));
        assert!(!r.healthy);
        assert_eq!(
            r.counts,
            Counts {
                ok: 1,
                warn: 1,
                fail: 1
            }
        );
        assert_eq!(r.counts.ok + r.counts.warn + r.counts.fail, r.checks.len());
    }

    #[test]
    fn disk_verdict_reads_the_kill_line_before_the_pause_line() {
        assert_eq!(disk_verdict(100, None, None), None);
        assert_eq!(
            disk_verdict(900, Some(1000), Some(500)),
            Some(DiskVerdict::BelowPause {
                free_mib: 900,
                line_mib: 1000
            })
        );
        assert_eq!(
            disk_verdict(400, Some(1000), Some(500)),
            Some(DiskVerdict::BelowKill {
                free_mib: 400,
                line_mib: 500
            })
        );
        assert_eq!(
            disk_verdict(2000, Some(1000), Some(500)),
            Some(DiskVerdict::Ok)
        );
        // A kill line without a pause line still applies.
        assert_eq!(
            disk_verdict(400, None, Some(500)),
            Some(DiskVerdict::BelowKill {
                free_mib: 400,
                line_mib: 500
            })
        );
        assert_eq!(disk_verdict(0, Some(0), Some(0)), Some(DiskVerdict::Ok));
    }

    #[test]
    fn probe_verdict_separates_missing_stale_and_fresh() {
        let now = 1_000_000_000;
        let six_h = 6 * 60 * 60 * 1000;
        assert_eq!(probe_verdict(&[], now, six_h), ProbeVerdict::NoData);
        assert_eq!(
            probe_verdict(&[snap("a", now - 60_000)], now, six_h),
            ProbeVerdict::Fresh { age_ms: 60_000 }
        );
        assert_eq!(
            probe_verdict(&[snap("a", now - 7 * 60 * 60 * 1000)], now, six_h),
            ProbeVerdict::Stale {
                age_ms: 7 * 60 * 60 * 1000
            }
        );
        // A capture stamped in the future is fresh, never a negative age.
        assert_eq!(
            probe_verdict(&[snap("a", now + 5_000)], now, six_h),
            ProbeVerdict::Fresh { age_ms: 0 }
        );
        // The newest window decides.
        assert_eq!(
            probe_verdict(
                &[snap("a", now - 9 * 60 * 60 * 1000), snap("a", now - 1_000)],
                now,
                six_h
            ),
            ProbeVerdict::Fresh { age_ms: 1_000 }
        );
    }

    #[test]
    fn format_mib_switches_to_gib_at_1024() {
        assert_eq!(format_mib(512), "512 MiB");
        assert_eq!(format_mib(1023), "1023 MiB");
        assert_eq!(format_mib(1024), "1.0 GiB");
        assert_eq!(format_mib(15 * 1024), "15.0 GiB");
    }

    #[test]
    fn version_mismatch_compares_exactly() {
        assert!(!version_mismatch("0.1.0", "0.1.0"));
        assert!(version_mismatch("0.1.0", "0.2.0"));
    }

    #[test]
    fn unknown_config_keys_names_sections_and_nested_keys() {
        let good = Config::default().to_toml_string().unwrap();
        assert_eq!(unknown_config_keys(&good), Ok(Vec::new()));
        assert_eq!(
            unknown_config_keys("[coordinator]\nurl = \"http://x\"\n"),
            Ok(Vec::new())
        );

        let typos = "[coordinatorr]\nurl = \"http://x\"\n\n[agent]\nsample_sec = 5\n";
        assert_eq!(
            unknown_config_keys(typos),
            Ok(vec!["agent.sample_sec".into(), "coordinatorr".into()])
        );

        let account =
            "[[quota.accounts]]\nname = \"work\"\nprovider = \"claude\"\nprboe = \"manual\"\n";
        assert_eq!(
            unknown_config_keys(account),
            Ok(vec!["quota.accounts.prboe".into()])
        );
    }

    #[test]
    fn unknown_config_keys_accepts_optional_and_guard_fields() {
        // Fields that default to None serialize away, so walking the defaults
        // used to flag them; the real parse keeps them.
        let text = "[agent]\ndevice_id = \"box\"\nprofile = \"linux_server\"\n\n\
                    [advisor]\nai_account = \"work\"\n\n\
                    [guard]\nload_1m_pause = 4.0\ndisk_free_mib_kill = 1024\n\n\
                    [[quota.accounts]]\nname = \"work\"\nprovider = \"claude\"\nprobe_path = \"/tmp/q.json\"\n";
        assert_eq!(unknown_config_keys(text), Ok(Vec::new()));
    }

    #[test]
    fn unknown_config_keys_is_a_parse_error_on_broken_toml() {
        assert!(matches!(
            unknown_config_keys("[agent\n"),
            Err(ConfigError::Parse(_))
        ));
    }
}
