//! Domain: the exit record a job supervisor leaves on disk, so an agent that died while a job
//! ran can still report the job's real exit after it restarts. No I/O.

use serde::{Deserialize, Serialize};

use super::ExitInfo;
use crate::types::UnixMs;

/// Bumped when the record changes shape; a record of another version is ignored.
const FORMAT: u32 = 1;

/// How one supervised job ended, as written by its supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitMarker {
    v: u32,
    /// The supervisor's pid, which is the job root's pid as the agent knows it.
    pub pid: u32,
    pub ended_ms: UnixMs,
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitMarker {
    pub fn new(pid: u32, ended_ms: UnixMs, exit: ExitInfo) -> Self {
        Self {
            v: FORMAT,
            pid,
            ended_ms,
            code: exit.code,
            signal: exit.signal,
        }
    }

    /// One JSON line. A reader never sees half of it: the writer renames a finished file.
    pub fn encode(&self) -> String {
        // Plain numbers and options cannot fail to serialize.
        serde_json::to_string(self).unwrap_or_default() + "\n"
    }

    /// `None` for a torn, unreadable or foreign-version record: nothing is guessed from it.
    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str::<Self>(text.trim())
            .ok()
            .filter(|m| m.v == FORMAT)
    }

    pub fn exit(&self) -> ExitInfo {
        ExitInfo {
            code: self.code,
            signal: self.signal,
        }
    }
}

/// What a restarted agent finds for a job that a previous agent left running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanVerdict {
    /// The supervisor recorded the job's real exit: report it.
    Exited(ExitMarker),
    /// The root is still the recorded process and nothing recorded an exit: it was killed here.
    Killed,
    /// Nothing is known, so nothing is reported: the lease expiry decides.
    Gone,
}

/// Decides from the recorded root pid, whether that root is still the same process, and the
/// marker found on disk. A marker written by another pid belongs to an earlier attempt.
pub fn orphan_verdict(
    root_pid: u32,
    root_is_same_process: bool,
    marker: Option<ExitMarker>,
) -> OrphanVerdict {
    match marker {
        Some(m) if m.pid == root_pid => OrphanVerdict::Exited(m),
        _ if root_is_same_process => OrphanVerdict::Killed,
        _ => OrphanVerdict::Gone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(pid: u32) -> ExitMarker {
        ExitMarker::new(
            pid,
            1_700_000_000_000,
            ExitInfo {
                code: Some(7),
                signal: None,
            },
        )
    }

    #[test]
    fn a_marker_survives_the_file_and_a_damaged_one_reads_as_nothing() {
        let m = marker(41);
        assert_eq!(ExitMarker::parse(&m.encode()), Some(m));
        assert_eq!(m.exit().code, Some(7));

        let line = m.encode();
        assert_eq!(ExitMarker::parse(&line[..line.len() / 2]), None, "torn");
        assert_eq!(ExitMarker::parse(""), None, "empty");
        let foreign = line.replace("\"v\":1", "\"v\":2");
        assert_eq!(ExitMarker::parse(&foreign), None, "another version");
    }

    #[test]
    fn a_restart_reports_a_recorded_exit_only_for_the_recorded_root() {
        let m = marker(41);
        let cases = [
            // The supervisor recorded the exit: the real exit wins, whether or not the root lingers.
            (41, false, Some(m), OrphanVerdict::Exited(m)),
            (41, true, Some(m), OrphanVerdict::Exited(m)),
            // A marker from another pid is an earlier attempt's.
            (42, true, Some(m), OrphanVerdict::Killed),
            (42, false, Some(m), OrphanVerdict::Gone),
            // No marker: a live root was killed here, a dead one is left to the lease.
            (41, true, None, OrphanVerdict::Killed),
            (41, false, None, OrphanVerdict::Gone),
        ];
        for (pid, same, found, want) in cases {
            assert_eq!(
                orphan_verdict(pid, same, found),
                want,
                "pid {pid} same {same}"
            );
        }
    }
}
