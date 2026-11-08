//! 24 h metrics ring buffer in `agent/metrics.jsonl`. Adapter layer: blocking file I/O only.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use idlemax_core::types::{MetricsSnapshot, UnixMs};

const RETENTION_SECS: u64 = 24 * 60 * 60;
pub const RETENTION_MS: UnixMs = 24 * 60 * 60 * 1000;
const FILE_NAME: &str = "metrics.jsonl";

#[derive(Debug, thiserror::Error)]
pub enum RingError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encode metrics: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Append-only JSON lines, rewritten to the last 24 h once it holds two days of samples.
#[derive(Debug)]
pub struct MetricsRing {
    path: PathBuf,
    lines: usize,
    compact_at: usize,
}

impl MetricsRing {
    pub fn open(dir: &Path, sample_secs: u64) -> Result<Self, RingError> {
        let path = dir.join(FILE_NAME);
        let lines = match File::open(&path) {
            Ok(f) => BufReader::new(f).lines().count(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => return Err(RingError::Io { path, source }),
        };
        let per_day = RETENTION_SECS / sample_secs.max(1);
        let per_day = usize::try_from(per_day).unwrap_or(usize::MAX).max(1);
        Ok(Self {
            path,
            lines,
            compact_at: per_day.saturating_mul(2),
        })
    }

    pub fn append(&mut self, m: &MetricsSnapshot, now_ms: UnixMs) -> Result<(), RingError> {
        let line = serde_json::to_string(m)?;
        let mut f = crate::state::private_file()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| self.io(e))?;
        writeln!(f, "{line}").map_err(|e| self.io(e))?;
        self.lines += 1;
        if self.lines >= self.compact_at {
            self.compact(now_ms)?;
        }
        Ok(())
    }

    /// Keeps samples newer than 24 h; drops unreadable lines. Atomic via rename.
    fn compact(&mut self, now_ms: UnixMs) -> Result<(), RingError> {
        let f = File::open(&self.path).map_err(|e| self.io(e))?;
        let kept: Vec<String> = BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter(|l| {
                serde_json::from_str::<MetricsSnapshot>(l)
                    .is_ok_and(|m| m.at_ms >= now_ms - RETENTION_MS)
            })
            .collect();
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut out = crate::state::private_file()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| self.io(e))?;
        for l in &kept {
            writeln!(out, "{l}").map_err(|e| self.io(e))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| self.io(e))?;
        self.lines = kept.len();
        Ok(())
    }

    fn io(&self, source: std::io::Error) -> RingError {
        RingError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use idlemax_core::types::MetricsSnapshot;

    use super::*;

    /// Host metrics are not for other users: the file is 0600, also after a compaction rewrites it.
    #[test]
    fn the_metrics_file_is_private_before_and_after_compaction() {
        let dir = tempfile::tempdir().unwrap();
        // A huge sample interval makes the ring compact after two samples.
        let mut ring = MetricsRing::open(dir.path(), u64::MAX).unwrap();
        let mode = || {
            std::fs::metadata(dir.path().join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        let now = 1_000_000;
        let sample = MetricsSnapshot {
            at_ms: now,
            ..MetricsSnapshot::default()
        };
        ring.append(&sample, now).unwrap();
        assert_eq!(mode(), 0o600, "first write");
        ring.append(&sample, now).unwrap();
        assert_eq!(ring.lines, 2, "compaction kept both samples");
        assert_eq!(mode(), 0o600, "after compaction");
    }
}
