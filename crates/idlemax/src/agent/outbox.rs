//! The agent's outbox: the domain `Outbox` backed by an append-only journal under
//! `agent/`. An update is on disk before anything sends it, and a restart replays whatever
//! the coordinator never acked. Adapter layer.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use idlemax_core::protocol::JobUpdate;
use idlemax_core::reconcile::{self, Outbox};
use idlemax_core::types::UnixMs;
use tracing::{error, info, warn};

const FILE: &str = "outbox.jsonl";
/// Job updates kept while the coordinator is unreachable.
pub const OUTBOX_CAP: usize = 1000;
/// The journal is rewritten once this many records are dead (acked, or acks) and they
/// outnumber the live ones, which bounds the file to about twice the cap.
const COMPACT_MIN_DEAD: usize = 256;

pub struct DurableOutbox {
    queue: Outbox,
    /// None after a disk error: updates still flow, but a crash would lose the queue.
    journal: Option<Journal>,
}

struct Journal {
    path: PathBuf,
    file: File,
    dead: usize,
}

impl DurableOutbox {
    /// Replays `dir/outbox.jsonl`, then rewrites it clean so a torn last line cannot
    /// swallow the next record. Never fails: on a disk error it logs and runs in memory.
    pub fn open(dir: &Path, cap: usize, now_ms: UnixMs) -> Self {
        let path = dir.join(FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                error!(path = %path.display(), error = %e, "cannot read the outbox journal, job updates will not survive a crash");
                return Self {
                    queue: Outbox::new(cap),
                    journal: None,
                };
            }
        };
        let replayed = reconcile::replay(&bytes);
        if replayed.skipped > 0 {
            warn!(
                skipped = replayed.skipped,
                "unreadable outbox journal lines skipped"
            );
        }
        // The wall clock keeps new sequence numbers above any used before a state wipe.
        let next_seq = replayed
            .max_seq
            .saturating_add(1)
            .max(u64::try_from(now_ms).unwrap_or(0));
        let queue = Outbox::restore(cap, next_seq, replayed.pending);
        if !queue.is_empty() {
            info!(
                pending = queue.len(),
                "replaying job updates the coordinator never acked"
            );
        }
        let journal = Journal::rewrite(path, &queue)
            .map_err(|e| error!(error = %e, "cannot write the outbox journal, job updates will not survive a crash"))
            .ok();
        Self { queue, journal }
    }

    /// Queues the update, durably, before the caller sends it.
    pub fn push(&mut self, update: JobUpdate) {
        let queued = self.queue.push(update);
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let written = reconcile::encode_update(&queued)
            .map_err(io::Error::other)
            .and_then(|line| journal.append(&line, true));
        if let Err(e) = written {
            // SHORTCUT: one failed journal write turns persistence off until restart, retry with a rewrite once disk errors prove transient.
            error!(error = %e, "outbox journal write failed, job updates will not survive a crash");
            self.journal = None;
        }
    }

    pub fn pending(&self, max: usize) -> Vec<JobUpdate> {
        self.queue.pending(max)
    }

    /// Forgets everything up to `seq`. The ack record is not synced: losing it only makes
    /// the next start replay updates the coordinator already applied, which it ignores.
    pub fn ack(&mut self, seq: u64) {
        let forgotten = self.queue.ack(seq);
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        let result = reconcile::encode_ack(seq)
            .map_err(io::Error::other)
            .and_then(|line| journal.append(&line, false))
            .and_then(|()| {
                journal.dead += forgotten + 1;
                if journal.dead >= COMPACT_MIN_DEAD.max(self.queue.len()) {
                    *journal = Journal::rewrite(journal.path.clone(), &self.queue)?;
                }
                Ok(())
            });
        if let Err(e) = result {
            error!(error = %e, "outbox journal write failed, job updates will not survive a crash");
            self.journal = None;
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl Journal {
    fn append(&mut self, line: &str, sync: bool) -> io::Result<()> {
        self.file.write_all(line.as_bytes())?;
        if sync {
            self.file.sync_data()?;
        }
        Ok(())
    }

    /// Replaces the journal with the live queue (temp file, then rename, so a crash leaves
    /// either the old or the new file) and opens it for appending.
    fn rewrite(path: PathBuf, queue: &Outbox) -> io::Result<Self> {
        let mut text = String::new();
        for update in queue.pending(usize::MAX) {
            text += &reconcile::encode_update(&update).map_err(io::Error::other)?;
        }
        let tmp = path.with_extension("jsonl.tmp");
        let mut file = private_options().write(true).truncate(true).open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        if let Some(dir) = path.parent() {
            // Best effort: persists the rename itself on filesystems that need it.
            let _ = File::open(dir).and_then(|d| d.sync_all());
        }
        let file = private_options().append(true).open(&path)?;
        Ok(Self {
            path,
            file,
            dead: 0,
        })
    }
}

/// Create-if-missing options with mode 0600: the journal holds job output.
fn private_options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts
}

#[cfg(test)]
mod tests {
    use idlemax_core::types::JobStatus;

    use super::*;

    const NOW: UnixMs = 1_800_000_000_000;

    fn finished(id: i64) -> JobUpdate {
        JobUpdate {
            id,
            device: "mac".into(),
            status: JobStatus::Succeeded,
            exit_code: Some(0),
            ..JobUpdate::default()
        }
    }

    #[test]
    fn unacked_updates_survive_a_crash_with_a_torn_tail_and_keep_their_seq() {
        let dir = tempfile::tempdir().unwrap();
        let mut outbox = DurableOutbox::open(dir.path(), 10, NOW);
        outbox.push(finished(1));
        outbox.push(finished(2));
        let first = outbox.pending(10)[0].seq;
        outbox.ack(first);
        // Simulate SIGKILL mid-append: no drop handlers run, half a record is on disk.
        std::mem::forget(outbox);
        let path = dir.path().join(FILE);
        let mut torn = OpenOptions::new().append(true).open(&path).unwrap();
        torn.write_all(br#"{"update":{"seq":99,"id":3"#).unwrap();

        let mut outbox = DurableOutbox::open(dir.path(), 10, NOW);
        let pending = outbox.pending(10);
        assert_eq!(pending.iter().map(|u| u.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(pending[0].seq, first + 1);

        outbox.push(finished(4));
        let seqs: Vec<u64> = outbox.pending(10).iter().map(|u| u.seq).collect();
        assert!(seqs[1] > seqs[0] && seqs[1] >= u64::try_from(NOW).unwrap());
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.ends_with('\n') && !on_disk.contains("\"seq\":99"));
    }
}
