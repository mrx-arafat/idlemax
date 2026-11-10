//! Adapter: check a leased job's cwd without taking the runner's process-control lock.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::runner::RunnerError;

pub(super) const CWD_CHECK_LIMIT: Duration = Duration::from_secs(2);
type Probe = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

#[derive(Clone)]
pub(super) struct CwdCheck {
    probe: Probe,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
    limit: Duration,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CwdError {
    #[error(transparent)]
    Resolve(#[from] RunnerError),
    #[error("cwd {} check timed out after {}ms", path.display(), limit.as_millis())]
    TimedOut { path: PathBuf, limit: Duration },
    #[error("cwd {}: still waiting on an earlier check", .0.display())]
    Pending(PathBuf),
    #[error("cwd {} check worker failed: {source}", path.display())]
    Worker {
        path: PathBuf,
        source: tokio::task::JoinError,
    },
}

impl CwdCheck {
    pub(super) fn new(limit: Duration, probe: Probe) -> Self {
        Self {
            probe,
            pending: Arc::default(),
            limit,
        }
    }

    pub(super) async fn check(&self, path: PathBuf) -> Result<PathBuf, CwdError> {
        if !self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.clone())
        {
            return Err(CwdError::Pending(path));
        }
        let flight = InFlight {
            path: path.clone(),
            pending: Arc::clone(&self.pending),
        };
        let probe = Arc::clone(&self.probe);
        let task = tokio::task::spawn_blocking(move || {
            let _flight = flight;
            probe(&_flight.path)
        });
        let answer = tokio::time::timeout(self.limit, task)
            .await
            .map_err(|_| CwdError::TimedOut {
                path: path.clone(),
                limit: self.limit,
            })?
            .map_err(|source| CwdError::Worker {
                path: path.clone(),
                source,
            })?;
        if answer {
            Ok(path)
        } else {
            Err(RunnerError::BadCwd(path).into())
        }
    }
}

/// A timed-out worker owns its path until it returns, including when the probe panics.
struct InFlight {
    path: PathBuf,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.path);
    }
}
