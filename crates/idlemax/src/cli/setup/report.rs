//! Delivery: the data `idlemax setup` reports, one step at a time.

use std::path::PathBuf;

use idlemax_core::doctor::Report;
use idlemax_core::install::setup::{Role, SetupPlan};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Changed something.
    Done,
    /// Already as wanted: nothing changed.
    AlreadyDone,
    Skipped,
    /// Dry run: this is what would change.
    WouldDo,
    Failed,
    /// Doctor ran and found failing checks (its own report lists the fixes).
    Problems,
    /// Services have not completed their handshake within the startup window.
    Starting,
}

#[derive(Debug, Serialize)]
pub struct StepReport {
    pub(super) step: &'static str,
    /// The service this step is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) which: Option<&'static str>,
    pub(super) status: Status,
    pub(super) detail: String,
    /// The command that does this by hand.
    pub(super) command: String,
    /// Files this step wrote, or would write.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) files: Vec<PathBuf>,
    /// Commands this step ran, or would run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) runs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) notes: Vec<String>,
}

impl StepReport {
    pub(super) fn new(
        step: &'static str,
        status: Status,
        detail: impl Into<String>,
        command: String,
    ) -> Self {
        Self {
            step,
            which: None,
            status,
            detail: detail.into(),
            command,
            files: Vec::new(),
            runs: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub(super) fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.files = files;
        self
    }

    pub(super) fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub(super) role: Role,
    pub(super) dry_run: bool,
    /// True when this run changed anything.
    pub(super) changed: bool,
    pub(super) steps: Vec<StepReport>,
    pub(super) doctor: Option<Report>,
    /// True when every step worked and doctor found no failing check.
    pub(super) ok: bool,
    pub(super) next: Vec<String>,
}

pub(super) fn finish(
    plan: &SetupPlan,
    dry_run: bool,
    steps: Vec<StepReport>,
    doctor: Option<Report>,
) -> SetupReport {
    let changed = steps.iter().any(|s| s.status == Status::Done);
    let stopped = steps.iter().any(|s| s.status == Status::Failed);
    // A run that installed no service leaves nothing running for doctor to find, so its
    // findings do not fail the command; with services, a failing doctor means it is not working.
    let doctor_fails =
        doctor.as_ref().is_some_and(|d| !d.healthy) && plan.service_skipped.is_none();
    let ok = !stopped && !doctor_fails;
    let mut next = Vec::new();
    if steps.iter().any(|s| s.status == Status::Starting) {
        next.push("idlemax doctor".to_string());
    } else if ok && !dry_run {
        next.push("idlemax tui".to_string());
        next.push("idlemax job add <spec.toml>".to_string());
    }
    SetupReport {
        role: plan.role,
        dry_run,
        changed: changed && !dry_run,
        steps,
        doctor,
        ok,
        next,
    }
}
