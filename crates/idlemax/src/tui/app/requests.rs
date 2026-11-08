//! Delivery: the requests a selection can make: control, cancel, schedules, advice and the
//! explanation of a waiting job.

use idlemax_core::explain::Explanation;
use idlemax_core::protocol::ControlRequest;
use idlemax_core::schedule::ScheduleId;
use idlemax_core::types::{ControlAction, ControlScope, JobId, RecommendationStatus, UnixMs};

use super::{App, Request, Tab};
use crate::tui::why::{explainable, WhyState};

impl App {
    pub(super) fn control_target(
        &mut self,
        action: ControlAction,
        now_ms: UnixMs,
    ) -> Option<Request> {
        match self.target() {
            Some((scope, _)) => Some(control(scope, action)),
            None => {
                self.flash("select a row first".into(), true, now_ms);
                None
            }
        }
    }

    pub(super) fn toggle_why(&mut self, now_ms: UnixMs) {
        if self.tab != Tab::Jobs || self.selected_job().is_none() {
            self.flash("w: select a job on the Jobs tab".into(), true, now_ms);
            return;
        }
        self.why_open = !self.why_open;
        // Reopening asks again, so the reasons are never older than the keypress.
        self.why_basis = None;
    }

    pub(super) fn scroll_why(&mut self, delta: isize) {
        if let Some(w) = self.why.as_mut().filter(|_| self.why_open) {
            w.scroll = w.scroll.saturating_add_signed(delta);
        }
    }

    /// Asks for the selected job's explanation when the pane is open and either the job
    /// or its batch headline changed since the last ask; browsing back to a job whose
    /// reasons are unchanged costs nothing.
    pub(super) fn request_why(&mut self) -> Option<Request> {
        if !self.why_open || self.tab != Tab::Jobs {
            return None;
        }
        let job = self.selected_job().filter(|j| explainable(j.status))?;
        let summary = self
            .data
            .as_ref()?
            .why
            .iter()
            .find(|w| w.job == job.id)
            .cloned();
        let basis = Some((job.id, summary));
        if self.why_basis == basis {
            return None;
        }
        let id = job.id;
        self.why_basis = basis;
        Some(Request::Why(id))
    }

    pub(super) fn store_why(&mut self, job: JobId, result: Result<Explanation, String>) {
        // Answers to a job the cursor already left are not worth showing.
        if self.why_basis.as_ref().map(|b| b.0) != Some(job) {
            return;
        }
        if result.is_err() {
            // Ask again on the next refresh.
            self.why_basis = None;
        }
        let scroll = self
            .why
            .as_ref()
            .filter(|w| w.job == job)
            .map_or(0, |w| w.scroll);
        self.why = Some(WhyState {
            job,
            result,
            scroll,
        });
    }

    /// Id and name of the schedule row the cursor is on.
    fn selected_schedule(&self) -> Option<(ScheduleId, String)> {
        let view = self
            .data
            .as_ref()?
            .schedules
            .get(self.selected(Tab::Schedules)?)?;
        Some((view.schedule.id, view.schedule.spec.name.clone()))
    }

    pub(super) fn schedule_request(
        &mut self,
        make: fn(ScheduleId) -> Request,
        now_ms: UnixMs,
    ) -> Option<Request> {
        let request = self.selected_schedule().map(|(id, _)| make(id));
        if request.is_none() {
            self.flash("select a row first".into(), true, now_ms);
        }
        request
    }

    pub(super) fn ask_remove_schedule(&mut self, now_ms: UnixMs) {
        match self.selected_schedule() {
            Some((id, name)) => self.ask(
                format!("Remove schedule {id} ({name})? A job it already queued keeps running."),
                Request::RemoveSchedule(id),
            ),
            None => self.flash("select a row first".into(), true, now_ms),
        }
    }

    pub(super) fn ask_cancel_job(&mut self, now_ms: UnixMs) {
        let job = (self.tab == Tab::Jobs)
            .then(|| self.selected(Tab::Jobs))
            .flatten()
            .and_then(|i| self.data.as_ref().map(|d| &d.jobs[i]))
            .filter(|j| !j.status.is_terminal())
            .map(|j| (j.id, j.spec.name.clone()));
        match job {
            Some((id, name)) => {
                self.ask(format!("Cancel job {id} ({name})?"), Request::CancelJob(id))
            }
            None => self.flash(
                "k: select a queued or running job on the Jobs tab".into(),
                true,
                now_ms,
            ),
        }
    }

    pub(super) fn advice_action(&mut self, accept: bool, now_ms: UnixMs) -> Option<Request> {
        let rec = (self.tab == Tab::Advice)
            .then(|| self.selected(Tab::Advice))
            .flatten()
            .and_then(|i| self.data.as_ref().map(|d| &d.advice[i]))
            .filter(|r| r.status == RecommendationStatus::Open);
        match (rec.map(|r| r.id), accept) {
            (Some(id), true) => Some(Request::AcceptAdvice(id)),
            (Some(id), false) => Some(Request::DismissAdvice(id)),
            (None, _) => {
                self.flash(
                    "a/d: select open advice on the Advice tab".into(),
                    true,
                    now_ms,
                );
                None
            }
        }
    }
}

pub(super) fn control(scope: ControlScope, action: ControlAction) -> Request {
    Request::Control(ControlRequest {
        scope,
        action,
        reason: Some("tui".into()),
    })
}
