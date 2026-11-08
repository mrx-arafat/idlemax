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
    /// Where the journal lives. None when an existing journal could not be read: it is left
    /// untouched (rewriting it would destroy updates not replayed), so nothing is persisted.
    path: Option<PathBuf>,
    /// None while the disk is failing: updates still flow, and every change retries a
    /// rewrite of the journal from the queue until one lands.
    journal: Option<Journal>,
}

struct Journal {
    path: PathBuf,
    file: File,
    dead: usize,
}

impl DurableOutbox {
    /// Replays `dir/outbox.jsonl`, then rewrites it clean so a torn last line cannot
    /// swallow the next record. Never fails: on a disk error it logs and runs in memory,
    /// retrying the journal on every change.
    pub fn open(dir: &Path, cap: usize, now_ms: UnixMs) -> Self {
        let path = dir.join(FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                error!(path = %path.display(), error = %e, "cannot read the outbox journal, job updates will not survive a crash");
                return Self {
                    queue: Outbox::new(cap),
                    path: None,
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
        let journal = Journal::rewrite(path.clone(), &queue)
            .map_err(|e| error!(error = %e, "cannot write the outbox journal, job updates will not survive a crash until it recovers"))
            .ok();
        Self {
            queue,
            path: Some(path),
            journal,
        }
    }

    /// Queues the update, durably, before the caller sends it.
    pub fn push(&mut self, update: JobUpdate) {
        let queued = self.queue.push(update);
        let line = reconcile::encode_update(&queued).map_err(io::Error::other);
        self.record(line, true, 0);
    }

    pub fn pending(&self, max: usize) -> Vec<JobUpdate> {
        self.queue.pending(max)
    }

    /// Forgets everything up to `seq`. The ack record is not synced: losing it only makes
    /// the next start replay updates the coordinator already applied, which it ignores.
    pub fn ack(&mut self, seq: u64) {
        let forgotten = self.queue.ack(seq);
        let line = reconcile::encode_ack(seq).map_err(io::Error::other);
        self.record(line, false, forgotten + 1);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl DurableOutbox {
    /// Appends one record. If that fails, or the journal is down, the journal is rewritten
    /// from the in-memory queue, which holds everything that must survive.
    fn record(&mut self, line: io::Result<String>, sync: bool, dead: usize) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let appended = match (self.journal.as_mut(), line) {
            (Some(journal), Ok(line)) => journal.append(&line, sync).map(|()| journal.dead += dead),
            (_, Err(e)) => Err(e),
            (None, Ok(_)) => Err(io::Error::other("the journal is down")),
        };
        match appended {
            Ok(()) => self.compact_if_due(),
            Err(cause) => self.repair(path, &cause),
        }
    }

    fn repair(&mut self, path: PathBuf, cause: &io::Error) {
        let was_down = self.journal.is_none();
        match Journal::rewrite(path, &self.queue) {
            Ok(journal) => {
                if was_down {
                    info!("outbox journal writable again");
                } else {
                    warn!(error = %cause, "outbox journal write failed, rewrote it from memory");
                }
                self.journal = Some(journal);
            }
            Err(e) => {
                if !was_down {
                    error!(error = %e, cause = %cause, "outbox journal write failed, job updates will not survive a crash until it recovers");
                }
                self.journal = None;
            }
        }
    }

    /// Drops dead records once they outnumber the live ones. A failed compaction leaves
    /// the old journal intact, so it only waits for the next batch of dead records.
    fn compact_if_due(&mut self) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        if journal.dead < COMPACT_MIN_DEAD.max(self.queue.len()) {
            return;
        }
        match Journal::rewrite(journal.path.clone(), &self.queue) {
            Ok(fresh) => *journal = fresh,
            Err(e) => {
                warn!(error = %e, "cannot compact the outbox journal");
                journal.dead = 0;
            }
        }
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

    #[test]
    fn a_failed_journal_write_is_repaired_and_a_dead_disk_is_retried_until_it_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let mut outbox = DurableOutbox::open(dir.path(), 10, NOW);
        outbox.push(finished(1));

        // The journal handle goes bad: appends fail, but the file can be rewritten.
        outbox.journal.as_mut().unwrap().file = File::open(&path).unwrap();
        outbox.push(finished(2));

        // The disk fails for a while: even the rewrite cannot create its temp file.
        outbox.journal.as_mut().unwrap().file = File::open(&path).unwrap();
        let blocker = path.with_extension("jsonl.tmp");
        std::fs::create_dir(&blocker).unwrap();
        outbox.push(finished(3));
        assert_eq!(
            outbox.len(),
            3,
            "updates keep flowing while the disk is down"
        );

        // The disk comes back: the next change rewrites the whole queue.
        std::fs::remove_dir(&blocker).unwrap();
        outbox.push(finished(4));
        std::mem::forget(outbox); // SIGKILL: no drop handlers run

        let reopened = DurableOutbox::open(dir.path(), 10, NOW);
        let ids: Vec<i64> = reopened.pending(10).iter().map(|u| u.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }
}
