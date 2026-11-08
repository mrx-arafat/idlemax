//! Delivery: dashboard state and key handling; turns keys into confirmed requests.

use idlemax_core::explain::WhySummary;
use idlemax_core::types::{ControlScope, Job, JobId, UnixMs};
use ratatui::widgets::TableState;

use super::feed::{Request, Snapshot, Update};
use super::why::WhyState;

mod keys;
mod requests;
#[cfg(test)]
mod tests;

const FLASH_MS: UnixMs = 6_000;

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
    /// The zone event times are shown in.
    pub zone: crate::cli::rows::Zone,
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
            zone: crate::cli::rows::Zone::Local,
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

    pub(super) fn ask(&mut self, prompt: String, request: Request) {
        self.confirm = Some(Confirm { prompt, request });
    }

    pub(super) fn flash(&mut self, text: String, error: bool, now_ms: UnixMs) {
        self.flash = Some(Flash {
            text,
            error,
            at_ms: now_ms,
        });
    }

    /// The job row the cursor is on.
    pub fn selected_job(&self) -> Option<&Job> {
        self.data.as_ref()?.jobs.get(self.selected(Tab::Jobs)?)
    }

    /// Keeps every table selection on an existing row.
    pub(super) fn clamp_selections(&mut self) {
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
