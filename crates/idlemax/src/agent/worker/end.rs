//! Adapter: what the agent loop tells the worker manager, and how a worker job ends once its
//! processes are gone.

use idlemax_core::reconcile::KillOrder;
use idlemax_core::types::{Job, JobId, JobStatus};

/// What the agent loop tells the manager.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Start(Box<Job>),
    Pause(JobId),
    Resume(JobId),
    Kill { id: JobId, order: KillOrder },
}

/// How a worker job ends, applied once its processes are gone.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct End {
    pub(super) status: JobStatus,
    pub(super) error: Option<String>,
    pub(super) retryable: bool,
    pub(super) owner_stop: bool,
    pub(super) end_text: Option<String>,
}

impl End {
    pub(super) fn failed(error: impl Into<String>) -> Self {
        Self {
            status: JobStatus::Failed,
            error: Some(error.into()),
            retryable: false,
            owner_stop: false,
            end_text: None,
        }
    }

    pub(super) fn killed(order: &KillOrder) -> Self {
        Self {
            status: JobStatus::Killed,
            error: Some(order.reason.clone()),
            retryable: order.retryable,
            owner_stop: order.owner_stop,
            end_text: None,
        }
    }

    /// A later kill replaces an earlier one only when it is the owner's stop, so a stop that
    /// arrives while a worker is already being shed is final and never relaunched (D11).
    pub(super) fn merge(&mut self, later: End) {
        if later.owner_stop && !self.owner_stop {
            let end_text = self.end_text.take();
            *self = later;
            self.end_text = self.end_text.take().or(end_text);
        }
    }
}
