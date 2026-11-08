//! Delivery: dashboard state and key handling; turns keys into confirmed requests.

use idlemax_core::explain::{Explanation, WhySummary};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::schedule::ScheduleId;
use idlemax_core::types::{ControlAction, ControlScope, Job, JobId, RecommendationStatus, UnixMs};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use super::feed::{Request, Snapshot, Update};
use super::why::{explainable, WhyState};

const FLASH_MS: UnixMs = 6_000;
/// Lines PgUp and PgDn move the explanation.
const WHY_SCROLL: isize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Devices,
    Jobs,
    Quota,
    Advice,
    Events,
    Schedules,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Devices,
        Tab::Jobs,
        Tab::Quota,
        Tab::Advice,
        Tab::Events,
        Tab::Schedules,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Devices => "Devices",
            Tab::Jobs => "Jobs",
            Tab::Quota => "Quota",
            Tab::Advice => "Advice",
            Tab::Events => "Events",
            Tab::Schedules => "Schedules",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub prompt: String,
    pub request: Request,
}

#[derive(Debug, Clone)]
pub struct Flash {
    pub text: String,
    pub error: bool,
    pub at_ms: UnixMs,
}

#[derive(Debug)]
pub struct App {
    pub source: String,
    pub tab: Tab,
    pub data: Option<Snapshot>,
    pub updated_ms: Option<UnixMs>,
    pub fetch_error: Option<String>,
    pub flash: Option<Flash>,
    pub confirm: Option<Confirm>,
    pub tables: [TableState; 6],
    pub quit: bool,
    /// The Jobs tab shows the reasons for the selected job.
    pub why_open: bool,
    pub why: Option<WhyState>,
    /// The job and batch headline the last explanation request was made for.
    why_basis: Option<(JobId, Option<WhySummary>)>,
}

impl App {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            tab: Tab::Devices,
            data: None,
            updated_ms: None,
            fetch_error: None,
            flash: None,
            confirm: None,
            tables: Default::default(),
            quit: false,
            why_open: false,
            why: None,
            why_basis: None,
        }
    }

    /// Takes in what the feed fetched. A new snapshot may ask for a fresh explanation.
    pub fn apply(&mut self, update: Update, now_ms: UnixMs) -> Option<Request> {
        match update {
            Update::Snapshot(snap, at) => {
                self.data = Some(*snap);
                self.updated_ms = Some(at);
                self.fetch_error = None;
                self.clamp_selections();
                return self.request_why();
            }
            Update::FetchFailed(e) => self.fetch_error = Some(e),
            Update::Done(text) => self.flash(format!("ok: {text}"), false, now_ms),
            Update::Failed(text) => self.flash(text, true, now_ms),
            Update::Why(job, result) => self.store_why(job, result),
        }
        None
    }

    /// Flash text still worth showing at `now_ms`.
    pub fn live_flash(&self, now_ms: UnixMs) -> Option<&Flash> {
        self.flash.as_ref().filter(|f| now_ms - f.at_ms < FLASH_MS)
    }

    pub fn rows(&self, tab: Tab) -> usize {
        self.data.as_ref().map_or(0, |d| match tab {
            Tab::Devices => d.status.devices.len(),
            Tab::Jobs => d.jobs.len(),
            Tab::Quota => d.quota.len(),
            Tab::Advice => d.advice.len(),
            Tab::Events => d.events.len(),
            Tab::Schedules => d.schedules.len(),
        })
    }

    pub fn selected(&self, tab: Tab) -> Option<usize> {
        self.tables[tab.index()]
            .selected()
            .filter(|i| *i < self.rows(tab))
    }

    /// Scope and label that p and r act on in the current tab.
    pub fn target(&self) -> Option<(ControlScope, String)> {
        let data = self.data.as_ref()?;
        let sel = self.selected(self.tab);
        match self.tab {
            Tab::Devices => sel.map(|i| {
                let id = data.status.devices[i].info.id.clone();
                (ControlScope::Device(id.clone()), format!("device {id}"))
            }),
            Tab::Jobs => sel.map(|i| {
                let id = data.jobs[i].id;
                (ControlScope::Job(id), format!("job {id}"))
            }),
            Tab::Quota => sel.map(|i| {
                let name = data.quota[i].account.clone();
                (
                    ControlScope::Account(name.clone()),
                    format!("account {name}"),
                )
            }),
            Tab::Schedules => None,
            Tab::Advice | Tab::Events => Some((ControlScope::Global, "everything".into())),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent, now_ms: UnixMs) -> Option<Request> {
        if let Some(confirm) = self.confirm.take() {
            return match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => Some(confirm.request),
                _ => {
                    self.flash("cancelled".into(), false, now_ms);
                    None
                }
            };
        }
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        match key.code {
            _ if ctrl_c => self.quit = true,
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Right => self.switch(1),
            KeyCode::BackTab | KeyCode::Left => self.switch(Tab::ALL.len() - 1),
            KeyCode::Char(c @ '1'..='6') => self.tab = Tab::ALL[usize::from(c as u8 - b'1')],
            KeyCode::Down => self.tables[self.tab.index()].select_next(),
            KeyCode::Up => self.tables[self.tab.index()].select_previous(),
            KeyCode::Char('p') if self.tab == Tab::Schedules => {
                return self.schedule_request(Request::PauseSchedule, now_ms)
            }
            KeyCode::Char('r') if self.tab == Tab::Schedules => {
                return self.schedule_request(Request::ResumeSchedule, now_ms)
            }
            KeyCode::Char('d') if self.tab == Tab::Schedules => self.ask_remove_schedule(now_ms),
            KeyCode::Char('p') => return self.control_target(ControlAction::Pause, now_ms),
            KeyCode::Char('r') => return self.control_target(ControlAction::Resume, now_ms),
            KeyCode::Char('P') => return Some(control(ControlScope::Global, ControlAction::Pause)),
            KeyCode::Char('R') => {
                return Some(control(ControlScope::Global, ControlAction::Resume))
            }
            KeyCode::Char('s') => self.ask(
                "Stop ALL jobs on every device and hold new ones?".into(),
                control(ControlScope::Global, ControlAction::StopAll),
            ),
            KeyCode::Char('k') => self.ask_cancel_job(now_ms),
            KeyCode::Char('w') => self.toggle_why(now_ms),
            KeyCode::PageDown => self.scroll_why(WHY_SCROLL),
            KeyCode::PageUp => self.scroll_why(-WHY_SCROLL),
            KeyCode::Char('a') => return self.advice_action(true, now_ms),
            KeyCode::Char('d') => return self.advice_action(false, now_ms),
            KeyCode::Char('f') if self.tab == Tab::Advice => return Some(Request::RefreshAdvice),
            _ => {}
        }
        self.clamp_selections();
        self.request_why()
    }

    fn switch(&mut self, step: usize) {
        self.tab = Tab::ALL[(self.tab.index() + step) % Tab::ALL.len()];
    }

    fn ask(&mut self, prompt: String, request: Request) {
        self.confirm = Some(Confirm { prompt, request });
    }

    fn flash(&mut self, text: String, error: bool, now_ms: UnixMs) {
        self.flash = Some(Flash {
            text,
            error,
            at_ms: now_ms,
        });
    }

    fn control_target(&mut self, action: ControlAction, now_ms: UnixMs) -> Option<Request> {
        match self.target() {
            Some((scope, _)) => Some(control(scope, action)),
            None => {
                self.flash("select a row first".into(), true, now_ms);
                None
            }
        }
    }

    /// The job row the cursor is on.
    pub fn selected_job(&self) -> Option<&Job> {
        self.data.as_ref()?.jobs.get(self.selected(Tab::Jobs)?)
    }

    fn toggle_why(&mut self, now_ms: UnixMs) {
        if self.tab != Tab::Jobs || self.selected_job().is_none() {
            self.flash("w: select a job on the Jobs tab".into(), true, now_ms);
            return;
        }
        self.why_open = !self.why_open;
        // Reopening asks again, so the reasons are never older than the keypress.
        self.why_basis = None;
    }

    fn scroll_why(&mut self, delta: isize) {
        if let Some(w) = self.why.as_mut().filter(|_| self.why_open) {
            w.scroll = w.scroll.saturating_add_signed(delta);
        }
    }

    /// Asks for the selected job's explanation when the pane is open and either the job
    /// or its batch headline changed since the last ask; browsing back to a job whose
    /// reasons are unchanged costs nothing.
    fn request_why(&mut self) -> Option<Request> {
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

    fn store_why(&mut self, job: JobId, result: Result<Explanation, String>) {
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

    fn schedule_request(
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

    fn ask_remove_schedule(&mut self, now_ms: UnixMs) {
        match self.selected_schedule() {
            Some((id, name)) => self.ask(
                format!("Remove schedule {id} ({name})? A job it already queued keeps running."),
                Request::RemoveSchedule(id),
            ),
            None => self.flash("select a row first".into(), true, now_ms),
        }
    }

    fn ask_cancel_job(&mut self, now_ms: UnixMs) {
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

    fn advice_action(&mut self, accept: bool, now_ms: UnixMs) -> Option<Request> {
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

    /// Keeps every table selection on an existing row.
    fn clamp_selections(&mut self) {
        for tab in Tab::ALL {
            let n = self.rows(tab);
            let state = &mut self.tables[tab.index()];
            let sel = match (n, state.selected()) {
                (0, _) => None,
                (_, None) => Some(0),
                (n, Some(i)) => Some(i.min(n - 1)),
            };
            state.select(sel);
        }
    }
}

fn control(scope: ControlScope, action: ControlAction) -> Request {
    Request::Control(ControlRequest {
        scope,
        action,
        reason: Some("tui".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn press(app: &mut App, c: char) -> Option<Request> {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), 0)
    }

    fn app_with_schedule() -> App {
        let view = json!({
            "id": 3, "spec": {"name": "repo-health", "command": ["true"]}, "every": "1d",
            "created_ms": 0, "last_fired_ms": null, "last_job": null,
            "decision": {"kind": "fire"}
        });
        let mut app = App::new("test");
        let snap = Snapshot {
            schedules: vec![serde_json::from_value(view).unwrap()],
            ..Snapshot::default()
        };
        app.apply(Update::Snapshot(Box::new(snap), 0), 0);
        app
    }

    #[test]
    fn schedules_tab_pauses_resumes_and_confirms_removal_of_the_selected_row() {
        let mut app = app_with_schedule();
        press(&mut app, '6');
        assert_eq!(app.tab, Tab::Schedules);
        assert_eq!(press(&mut app, 'p'), Some(Request::PauseSchedule(3)));
        assert_eq!(press(&mut app, 'r'), Some(Request::ResumeSchedule(3)));

        assert_eq!(press(&mut app, 'd'), None);
        let prompt = app.confirm.as_ref().map(|c| c.prompt.as_str());
        assert_eq!(
            prompt,
            Some("Remove schedule 3 (repo-health)? A job it already queued keeps running.")
        );
        assert_eq!(press(&mut app, 'y'), Some(Request::RemoveSchedule(3)));
    }

    fn jobs_with_headline(message: &str) -> Snapshot {
        let job = |id: i64, status: &str| json!({"id": id, "spec": {"name": format!("job-{id}"), "command": ["true"]}, "status": status});
        let summary = json!({"job": 1, "status": "queued", "state": "blocked", "more": 0, "headline": {
            "device": null,
            "reason": {"kind": "guard", "message": message, "fix": null, "until_ms": null}}});
        Snapshot {
            jobs: serde_json::from_value(json!([job(1, "queued"), job(2, "running")])).unwrap(),
            why: serde_json::from_value(json!([summary])).unwrap(),
            ..Snapshot::default()
        }
    }

    fn explanation(job: i64) -> Explanation {
        serde_json::from_value(json!({"job": job, "name": "job", "status": "queued", "state": "blocked",
            "summary": "Job 1 is queued but not starting.", "reasons": [], "devices": [], "at_ms": 0}))
        .unwrap()
    }

    #[test]
    fn why_pane_asks_for_the_selected_waiting_job_only_when_its_reasons_changed() {
        let mut app = App::new("test");
        app.apply(
            Update::Snapshot(Box::new(jobs_with_headline("on battery")), 0),
            0,
        );
        app.tab = Tab::Jobs;
        assert_eq!(press(&mut app, 'w'), Some(Request::Why(1)));
        app.apply(Update::Why(1, Ok(explanation(1))), 0);
        assert_eq!(app.why.as_ref().map(|w| w.job), Some(1));

        // A running job has nothing to explain; coming back to job 1 is free.
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.on_key(down, 0), None);
        assert_eq!(app.on_key(up, 0), None);

        // The same reasons on the next refresh cost nothing; changed reasons are fetched again.
        let same = Update::Snapshot(Box::new(jobs_with_headline("on battery")), 0);
        assert_eq!(app.apply(same, 0), None);
        let changed = Update::Snapshot(Box::new(jobs_with_headline("load is high")), 0);
        assert_eq!(app.apply(changed, 0), Some(Request::Why(1)));

        // Closing and reopening asks again.
        assert_eq!(press(&mut app, 'w'), None);
        assert_eq!(press(&mut app, 'w'), Some(Request::Why(1)));
    }
}
