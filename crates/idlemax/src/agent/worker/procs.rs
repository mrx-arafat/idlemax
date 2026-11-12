//! Adapter: what the worker manager needs from the OS process table: a process's environment,
//! the member pids of a worker, which saved pids still live, and signals by pid. `OsProcs`
//! wraps the runner's `ProcTable`; `FakeProcs` stands in for it in tests.

use idlemax_core::job_run::JobKey;
use idlemax_core::types::UnixMs;

use crate::agent::runner::os::{self, ProcTable, Sig};

/// A pid with its start time, so a reused pid is never taken for a worker's process.
pub type Member = (u32, Option<UnixMs>);

pub trait Procs: Send {
    /// The variables of `pid`; None when it is gone or its environment is hidden.
    fn environ(&mut self, pid: u32) -> Option<Vec<(String, String)>>;
    /// The worker's processes: the shell, its group and every process carrying `tag`.
    fn members(&mut self, shell: u32, tag: u64, started_ms: UnixMs) -> Vec<Member>;
    /// Which of `members` are still the same live processes, plus any process with `tag`.
    fn alive(&mut self, members: &[Member], tag: u64, started_ms: UnixMs) -> Vec<u32>;
    /// Resident memory of `pids` together.
    fn rss(&mut self, pids: &[u32]) -> u64;
    fn signal(&mut self, pids: &[u32], sig: Sig);
    /// The `n` biggest memory users outside `exclude`, as one line ("Safari 2.1 GiB, ...").
    fn top_memory(&mut self, n: usize, exclude: &[u32]) -> String;
}

pub struct OsProcs {
    table: ProcTable,
}

impl OsProcs {
    pub fn new() -> Option<Self> {
        ProcTable::new().map(|table| Self { table })
    }

    fn key(shell: u32, tag: u64, started_ms: UnixMs) -> JobKey {
        JobKey {
            id: 0,
            root: shell,
            started_ms,
            tag,
        }
    }
}

impl Procs for OsProcs {
    fn environ(&mut self, pid: u32) -> Option<Vec<(String, String)>> {
        self.table.environ(pid)
    }

    fn members(&mut self, shell: u32, tag: u64, started_ms: UnixMs) -> Vec<Member> {
        self.table.refresh();
        let key = Self::key(shell, tag, started_ms);
        let found = self.table.usages(std::slice::from_ref(&key));
        let mut pids = vec![shell];
        if let Some(procs) = found.get(&shell) {
            pids.extend(&procs.outside_group);
        }
        pids.sort_unstable();
        pids.dedup();
        pids.into_iter()
            .map(|pid| (pid, self.table.start_ms(pid)))
            .collect()
    }

    fn alive(&mut self, members: &[Member], tag: u64, started_ms: UnixMs) -> Vec<u32> {
        let mut live: Vec<u32> = members
            .iter()
            .filter(|(pid, start)| start.is_some() && self.table.start_ms(*pid) == *start)
            .map(|(pid, _)| *pid)
            .collect();
        // Found by tag, since `usages` returns nothing once the shell has exited.
        let key = Self::key(0, tag, started_ms);
        live.extend(self.table.orphans(&key, false).outside_group);
        live.sort_unstable();
        live.dedup();
        live
    }

    fn rss(&mut self, pids: &[u32]) -> u64 {
        self.table.rss_of(pids)
    }

    fn signal(&mut self, pids: &[u32], sig: Sig) {
        os::signal_pids(pids, sig);
    }

    fn top_memory(&mut self, n: usize, exclude: &[u32]) -> String {
        self.table.refresh();
        let top = self.table.top_memory(n, exclude);
        let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
        let named: Vec<String> = top
            .iter()
            .map(|(name, rss)| format!("{name} {:.1} GiB", gib(*rss)))
            .collect();
        named.join(", ")
    }
}

/// A scripted process table: `live` is what is running, `environs` what each pid holds.
#[cfg(test)]
#[derive(Default)]
pub struct FakeProcs {
    pub live: std::collections::BTreeSet<u32>,
    pub environs: std::collections::HashMap<u32, Vec<(String, String)>>,
    /// Pids that survive a close and leave only on SIGKILL.
    pub stubborn: std::collections::BTreeSet<u32>,
    /// Pids that carry the worker's tag (a running setup or check).
    pub tagged: std::collections::BTreeSet<u32>,
    pub signals: Vec<(Vec<u32>, Sig)>,
}

#[cfg(test)]
impl Procs for FakeProcs {
    fn environ(&mut self, pid: u32) -> Option<Vec<(String, String)>> {
        self.environs.get(&pid).cloned()
    }

    fn members(&mut self, shell: u32, _tag: u64, _started_ms: UnixMs) -> Vec<Member> {
        self.live
            .iter()
            .filter(|&&pid| pid >= shell)
            .map(|&pid| (pid, Some(1)))
            .collect()
    }

    fn alive(&mut self, members: &[Member], _tag: u64, _started_ms: UnixMs) -> Vec<u32> {
        let mut live: Vec<u32> = members
            .iter()
            .map(|(pid, _)| *pid)
            .filter(|pid| self.live.contains(pid))
            .collect();
        live.extend(&self.tagged);
        live
    }

    fn rss(&mut self, pids: &[u32]) -> u64 {
        pids.len() as u64 * 1024
    }

    fn top_memory(&mut self, _n: usize, _exclude: &[u32]) -> String {
        "Safari 2.1 GiB".into()
    }

    fn signal(&mut self, pids: &[u32], sig: Sig) {
        if sig == Sig::Kill {
            for pid in pids {
                self.live.remove(pid);
                self.stubborn.remove(pid);
                self.tagged.remove(pid);
            }
        }
        self.signals.push((pids.to_vec(), sig));
    }
}
