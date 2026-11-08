//! Domain: pure decisions for running one job on a device (admission, throttle argv,
//! run clock, cap checks, exit outcome, rate-limit scan). No I/O, no clock.

pub mod inbox;
mod usage;
mod wrap;

pub use usage::{
    could_belong, job_procs, poll_every, JobKey, JobProcs, ProcFact, Usage, POLL_NEAR_CAP,
    POLL_RELAXED,
};
pub use wrap::{scope_unit, supports_scope_oom_policy, Wrapper};

use crate::types::{CostClass, JobSpec, JobStatus, UnixMs};

/// SIGTERM to SIGKILL grace.
pub const KILL_GRACE_MS: i64 = 15_000;
/// A recorded job start and an OS process start this close are the same process.
pub const SAME_PROCESS_TOLERANCE_MS: i64 = 5_000;

const MIB: u64 = 1024 * 1024;
/// Env prefixes that inject code into the wrapper or the allowlisted program.
const FORBIDDEN_ENV_PREFIXES: [&str; 2] = ["LD_", "DYLD_"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmitError {
    #[error("command is empty")]
    EmptyCommand,
    #[error("command {0:?} is not in agent.command_allowlist")]
    NotAllowed(String),
    #[error("env key {0:?} is not allowed in a job spec")]
    ForbiddenEnv(String),
}

/// True when `program` (argv[0]) equals an allowlist entry. Empty allows nothing.
pub fn allows_program(allowlist: &[String], program: &str) -> bool {
    allowlist.iter().any(|a| a == program)
}

/// Checks a spec against the device allowlist; argv[0] must equal an entry.
pub fn admit(spec: &JobSpec, allowlist: &[String]) -> Result<(), AdmitError> {
    let program = spec.command.first().map(String::as_str).unwrap_or_default();
    if program.trim().is_empty() {
        return Err(AdmitError::EmptyCommand);
    }
    if !allows_program(allowlist, program) {
        return Err(AdmitError::NotAllowed(program.to_string()));
    }
    match spec.env.keys().find(|k| forbidden_env(k)) {
        Some(key) => Err(AdmitError::ForbiddenEnv(key.clone())),
        None => Ok(()),
    }
}

fn forbidden_env(key: &str) -> bool {
    key == crate::TOKEN_ENV || FORBIDDEN_ENV_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// Pure runtime accounting using adapter-supplied awake milliseconds.
/// System sleep and explicitly paused spans do not consume the runtime cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunClock {
    started_ms: UnixMs,
    started_awake_ms: i64,
    paused_ms: i64,
    paused_since_ms: Option<i64>,
}

impl RunClock {
    pub fn start(now_ms: UnixMs, awake_ms: i64) -> Self {
        Self {
            started_ms: now_ms,
            started_awake_ms: awake_ms,
            paused_ms: 0,
            paused_since_ms: None,
        }
    }

    pub fn started_ms(&self) -> UnixMs {
        self.started_ms
    }

    pub fn is_paused(&self) -> bool {
        self.paused_since_ms.is_some()
    }

    pub fn pause(&mut self, awake_ms: i64) {
        self.paused_since_ms.get_or_insert(awake_ms);
    }

    pub fn resume(&mut self, awake_ms: i64) {
        if let Some(since) = self.paused_since_ms.take() {
            self.paused_ms += (awake_ms - since).max(0);
        }
    }

    pub fn active_ms(&self, awake_ms: i64) -> i64 {
        let open = self
            .paused_since_ms
            .map_or(0, |since| (awake_ms - since).max(0));
        (awake_ms - self.started_awake_ms - self.paused_ms - open).max(0)
    }
}

/// Why the runner is stopping a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The agent asked (guard, coordinator command, local stop); it supplies the text.
    Requested,
    MaxRuntime {
        limit_secs: u64,
    },
    MemoryCap {
        rss_mib: u64,
        cap_mib: u64,
    },
    /// Still running from a previous agent process.
    Orphaned,
}

impl StopReason {
    pub fn message(&self) -> Option<String> {
        match self {
            StopReason::Requested => None,
            StopReason::MaxRuntime { limit_secs } => {
                Some(format!("max runtime of {limit_secs} s exceeded"))
            }
            StopReason::MemoryCap { rss_mib, cap_mib } => {
                Some(format!("memory {rss_mib} MiB over the {cap_mib} MiB cap"))
            }
            StopReason::Orphaned => Some("agent restarted; orphaned job killed".into()),
        }
    }
}

/// First cap the job breaks, if any. `rss_bytes` covers the whole process tree.
pub fn breach(spec: &JobSpec, active_ms: i64, rss_bytes: Option<u64>) -> Option<StopReason> {
    let limit_ms = i64::try_from(spec.max_runtime_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    if active_ms >= limit_ms {
        return Some(StopReason::MaxRuntime {
            limit_secs: spec.max_runtime_secs,
        });
    }
    match rss_bytes {
        Some(rss) if rss > spec.mem_mb.saturating_mul(MIB) => Some(StopReason::MemoryCap {
            rss_mib: rss / MIB,
            cap_mib: spec.mem_mb,
        }),
        _ => None,
    }
}

/// SIGTERM was sent at `term_sent_ms`; SIGKILL is due once the grace has passed.
pub fn kill_due(term_sent_ms: UnixMs, now_ms: UnixMs) -> bool {
    now_ms - term_sent_ms >= KILL_GRACE_MS
}

/// A pid file entry still names the same live process (guards against pid reuse).
pub fn same_process(recorded_start_ms: UnixMs, os_start_ms: UnixMs) -> bool {
    (recorded_start_ms - os_start_ms).abs() <= SAME_PROCESS_TOLERANCE_MS
}

/// How the root process ended: exit code, or the signal that terminated it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

const SIGKILL: i32 = 9;

/// Terminal status for an ended job. A stop the runner sent always reads as Killed.
/// `memory_cap_mib` is the hard OS memory limit the job ran under, if any: there a SIGKILL
/// nobody sent is the kernel enforcing it.
pub fn outcome(exit: ExitInfo, stop: Option<&StopReason>, memory_cap_mib: Option<u64>) -> Outcome {
    let (status, error) = match (stop, exit.code, exit.signal) {
        (Some(reason), _, _) => (JobStatus::Killed, reason.message()),
        (None, Some(0), _) => (JobStatus::Succeeded, None),
        (None, Some(code), _) => (JobStatus::Failed, Some(format!("exit code {code}"))),
        (None, None, Some(SIGKILL)) => match memory_cap_mib {
            Some(cap) => (
                JobStatus::Killed,
                Some(format!("killed by the {cap} MiB memory cap")),
            ),
            None => (
                JobStatus::Failed,
                Some("killed by SIGKILL (memory cap or OOM killer)".into()),
            ),
        },
        (None, None, Some(sig)) => (
            JobStatus::Failed,
            Some(format!("terminated by signal {sig}")),
        ),
        (None, None, None) => (JobStatus::Failed, Some("ended without an exit code".into())),
    };
    Outcome {
        status,
        exit_code: exit.code,
        error,
    }
}

const LIMIT_PHRASES: [&str; 6] = [
    "usage limit",
    "rate limit",
    "rate_limit",
    "too many requests",
    "limit reached",
    "hit your limit",
];
const HTTP_CONTEXT: [&str; 4] = ["error", "status", "http", "code"];

/// AI job output mentions a usage limit or HTTP 429. Errs toward true: a false
/// positive only idles the account for the rest of its window.
pub fn rate_limited(cost: CostClass, output: &str) -> bool {
    cost == CostClass::Ai && output.lines().any(line_rate_limited)
}

fn line_rate_limited(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    LIMIT_PHRASES.iter().any(|p| l.contains(p))
        || (has_number(&l, "429") && HTTP_CONTEXT.iter().any(|w| l.contains(w)))
}

/// `num` appears with no digit directly before or after it.
fn has_number(text: &str, num: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(num).any(|(i, _)| {
        let before = i.checked_sub(1).and_then(|j| bytes.get(j));
        let after = bytes.get(i + num.len());
        !before.is_some_and(u8::is_ascii_digit) && !after.is_some_and(u8::is_ascii_digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn spec(cmd: &[&str]) -> JobSpec {
        JobSpec {
            name: "t".into(),
            command: strings(cmd),
            ..JobSpec::default()
        }
    }

    #[test]
    fn admit_needs_exact_allowlist_match_and_clean_env() {
        let allow = strings(&["git", "/usr/bin/make"]);
        assert_eq!(admit(&spec(&["git", "status"]), &allow), Ok(()));
        assert_eq!(
            admit(&spec(&["/usr/bin/git"]), &allow),
            Err(AdmitError::NotAllowed("/usr/bin/git".into()))
        );
        assert_eq!(admit(&spec(&[" "]), &allow), Err(AdmitError::EmptyCommand));
        assert_eq!(
            admit(&spec(&["git"]), &[]),
            Err(AdmitError::NotAllowed("git".into()))
        );
        let mut s = spec(&["git"]);
        s.env
            .insert("DYLD_INSERT_LIBRARIES".into(), "/tmp/x.dylib".into());
        assert_eq!(
            admit(&s, &allow),
            Err(AdmitError::ForbiddenEnv("DYLD_INSERT_LIBRARIES".into()))
        );
    }

    #[test]
    fn a_sigkill_under_a_hard_memory_cap_reads_as_killed_by_the_cap() {
        let sigkill = ExitInfo {
            code: None,
            signal: Some(9),
        };
        let capped = outcome(sigkill, None, Some(128));
        assert_eq!(capped.status, JobStatus::Killed);
        assert_eq!(
            capped.error.as_deref(),
            Some("killed by the 128 MiB memory cap")
        );

        // Without a hard cap the cause is unknown, and a stop the runner sent wins.
        let unknown = outcome(sigkill, None, None);
        assert_eq!(unknown.status, JobStatus::Failed);
        assert!(unknown.error.unwrap().contains("SIGKILL"));
        let stopped = outcome(sigkill, Some(&StopReason::Requested), Some(128));
        assert_eq!((stopped.status, stopped.error), (JobStatus::Killed, None));
        let exited = outcome(
            ExitInfo {
                code: Some(2),
                signal: None,
            },
            None,
            Some(128),
        );
        assert_eq!(exited.error.as_deref(), Some("exit code 2"));
    }

    #[test]
    fn run_clock_and_breach_measure_awake_time_and_ignore_sleep() {
        let started_wall = 1_700_000_000_000;
        let mut clock = RunClock::start(started_wall, 0);

        // System sleeps for 2 hours, but awake clock only advances by 500 ms.
        let awake_500ms = 500;
        assert_eq!(clock.active_ms(awake_500ms), 500);

        let mut s = spec(&["git"]);
        s.max_runtime_secs = 2;
        assert_eq!(breach(&s, clock.active_ms(awake_500ms), None), None);

        // Paused for 300 ms awake time.
        clock.pause(800);
        assert!(clock.is_paused());
        assert_eq!(clock.active_ms(1_000), 800);
        clock.resume(1_100);
        assert!(!clock.is_paused());
        assert_eq!(clock.active_ms(1_100), 800);

        // Awake time reaches 2301 ms (active = 2301 - 300 = 2001 ms > 2000 ms limit).
        let awake_past = 2_301;
        let active = clock.active_ms(awake_past);
        assert_eq!(active, 2_001);
        assert_eq!(
            breach(&s, active, None),
            Some(StopReason::MaxRuntime { limit_secs: 2 })
        );
    }
}
