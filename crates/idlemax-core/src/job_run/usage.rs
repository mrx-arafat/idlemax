//! Domain: which OS processes belong to a running job, their summed usage, and how
//! often to poll them. Pure: the adapter reports a process table snapshot.

use std::collections::HashMap;
use std::time::Duration;

use crate::types::{JobId, UnixMs};

/// Poll period while every job is well under its memory cap.
pub const POLL_RELAXED: Duration = Duration::from_secs(5);
/// Poll period while a job is near its cap or not measured yet.
pub const POLL_NEAR_CAP: Duration = Duration::from_secs(1);

/// A job is near its cap from this share of it.
const NEAR_CAP_PCT: u64 = 50;
const MIB: u64 = 1024 * 1024;
/// Process start times are whole seconds; allow this much rounding against the job start.
const START_SLACK_SECS: u64 = 2;

/// Memory and CPU of a job's processes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub rss_bytes: u64,
    pub cpu_pct: f64,
}

/// One row of the OS process table.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProcFact {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub pgid: Option<u32>,
    /// Whole seconds since the epoch.
    pub start_secs: u64,
    /// The attempt tag in the process's `IDLEMAX_JOB_TAG` env, when readable.
    pub tag: Option<u64>,
    pub rss_bytes: u64,
    pub cpu_pct: f64,
}

/// The job being measured. Its root pid is also its process group id.
#[derive(Debug, Clone, Copy)]
pub struct JobKey {
    pub id: JobId,
    pub root: u32,
    pub started_ms: UnixMs,
    /// Random per attempt. Job ids repeat across agents on one host; this does not.
    pub tag: u64,
}

/// A job's processes: summed usage, and the members a group signal cannot reach.
#[derive(Debug, Clone, PartialEq)]
pub struct JobProcs {
    pub usage: Usage,
    /// Members outside the job's process group (they called `setsid` or `setpgid`).
    pub outside_group: Vec<u32>,
}

/// A process that started since the job did may carry the job's tag.
pub fn could_belong(start_secs: u64, job_started_ms: UnixMs) -> bool {
    let job_secs = u64::try_from(job_started_ms / 1000).unwrap_or(0);
    start_secs.saturating_add(START_SLACK_SECS) >= job_secs
}

/// Everything the job runs: its process group (a plain double fork keeps it), anything
/// that carries its env tag (a `setsid` daemon leaves the group but keeps its env), and
/// the descendants of both. `None` while the root is gone.
pub fn job_procs(job: &JobKey, procs: &[ProcFact]) -> Option<JobProcs> {
    procs
        .iter()
        .any(|p| p.pid == job.root)
        .then(|| members(job, procs, true))
}

/// What a job left behind after its agent died. The group counts only when the recorded
/// root is still the same process (`root_trusted`): a reused pid would name a stranger's
/// group. Env-tagged processes and their descendants always count, and `outside_group`
/// then lists every member to signal by pid.
pub fn orphan_procs(job: &JobKey, procs: &[ProcFact], root_trusted: bool) -> JobProcs {
    members(job, procs, root_trusted)
}

fn members(job: &JobKey, procs: &[ProcFact], by_group: bool) -> JobProcs {
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, p) in procs.iter().enumerate() {
        if let Some(parent) = p.ppid {
            children.entry(parent).or_default().push(i);
        }
    }
    let mut stack: Vec<usize> = (0..procs.len())
        .filter(|&i| is_seed(job, &procs[i], by_group))
        .collect();
    let mut seen = vec![false; procs.len()];
    let (mut usage, mut outside_group) = (Usage::default(), Vec::new());
    while let Some(i) = stack.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        let p = &procs[i];
        usage.rss_bytes = usage.rss_bytes.saturating_add(p.rss_bytes);
        usage.cpu_pct += p.cpu_pct;
        if !(by_group && p.pgid == Some(job.root)) {
            outside_group.push(p.pid);
        }
        stack.extend(children.get(&p.pid).into_iter().flatten());
    }
    JobProcs {
        usage,
        outside_group,
    }
}

fn is_seed(job: &JobKey, p: &ProcFact, by_group: bool) -> bool {
    (by_group && (p.pid == job.root || p.pgid == Some(job.root)))
        || (p.tag == Some(job.tag) && could_belong(p.start_secs, job.started_ms))
}

/// How long to wait before the next memory check. Each job is `(rss_bytes, cap_mb)`;
/// an unmeasured job counts as near its cap so its first reading comes quickly.
pub fn poll_every(jobs: impl IntoIterator<Item = (Option<u64>, u64)>) -> Duration {
    if jobs.into_iter().any(|(rss, cap_mb)| near_cap(rss, cap_mb)) {
        POLL_NEAR_CAP
    } else {
        POLL_RELAXED
    }
}

fn near_cap(rss_bytes: Option<u64>, cap_mb: u64) -> bool {
    rss_bytes.is_none_or(|rss| {
        rss.saturating_mul(100) >= cap_mb.saturating_mul(MIB).saturating_mul(NEAR_CAP_PCT)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: JobKey = JobKey {
        id: 7,
        root: 100,
        started_ms: 1_000_000,
        tag: 0x7a6,
    };
    const STARTED_SECS: u64 = 1_000;

    fn fact(pid: u32, ppid: u32, pgid: u32, rss: u64) -> ProcFact {
        ProcFact {
            pid,
            ppid: Some(ppid),
            pgid: Some(pgid),
            start_secs: STARTED_SECS + 1,
            rss_bytes: rss,
            ..ProcFact::default()
        }
    }

    fn tagged(mut p: ProcFact, tag: u64, start_secs: u64) -> ProcFact {
        p.tag = Some(tag);
        p.start_secs = start_secs;
        p
    }

    #[test]
    fn job_procs_follow_the_group_the_tree_and_the_env_tag() {
        let procs = [
            fact(100, 1, 100, 1),
            fact(101, 100, 100, 2),
            // Plain double fork: reparented to init, still in the job's group.
            fact(102, 1, 100, 4),
            // Child that moved to its own group, parent still alive.
            fact(103, 100, 103, 8),
            // setsid daemon: own group, reparented, found by its env tag.
            tagged(fact(104, 1, 104, 16), JOB.tag, STARTED_SECS),
            fact(105, 104, 105, 32),
            // Not ours: another attempt's tag (even of a job with the same id), a tag from
            // before this job started, no tag.
            tagged(fact(106, 1, 106, 1 << 20), JOB.tag + 1, STARTED_SECS),
            tagged(fact(107, 1, 107, 1 << 20), JOB.tag, STARTED_SECS - 10),
            fact(108, 1, 108, 1 << 20),
        ];
        let found = job_procs(&JOB, &procs).unwrap();
        assert_eq!(found.usage.rss_bytes, 1 + 2 + 4 + 8 + 16 + 32);
        let mut outside = found.outside_group;
        outside.sort_unstable();
        assert_eq!(outside, [103, 104, 105]);
        assert_eq!(job_procs(&JOB, &procs[1..]), None, "root gone");
    }

    #[test]
    fn orphan_procs_trust_the_group_only_while_the_root_is_the_same_process() {
        let procs = [
            fact(102, 1, 100, 4),
            tagged(fact(104, 1, 104, 16), JOB.tag, STARTED_SECS),
            fact(108, 1, 108, 1 << 20),
        ];
        // Root died and its pid may be reused: only the tagged daemon is ours.
        let gone = orphan_procs(&JOB, &procs, false);
        assert_eq!(gone.outside_group, [104]);
        // Root still the same process: the group is ours too, signalled as a group.
        let alive = orphan_procs(&JOB, &procs, true);
        assert_eq!(alive.outside_group, [104]);
        assert_eq!(alive.usage.rss_bytes, 4 + 16);
    }

    #[test]
    fn poll_every_speeds_up_near_the_cap_or_when_unmeasured() {
        let cap = 64;
        let mib = |n: u64| Some(n * MIB);
        assert_eq!(poll_every([]), POLL_RELAXED);
        assert_eq!(poll_every([(mib(31), cap)]), POLL_RELAXED);
        assert_eq!(poll_every([(mib(32), cap)]), POLL_NEAR_CAP);
        assert_eq!(poll_every([(None, cap)]), POLL_NEAR_CAP);
        assert_eq!(
            poll_every([(mib(1), cap), (mib(60), cap)]),
            POLL_NEAR_CAP,
            "one near job speeds up the poll"
        );
    }
}
