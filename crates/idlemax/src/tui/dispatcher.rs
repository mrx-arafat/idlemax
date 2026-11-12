//! Delivery: the Dispatch tab's state and keys: which pane shows, the limit typed in the
//! footer, and the confirmed requests its keys make. Drawing is in `dispatcher_view`.

use idlemax_core::dispatch::view::DispatcherView;
use idlemax_core::types::UnixMs;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::app::{App, Tab};
use super::feed::{Request, Snapshot};
use idlemax_core::dispatch::proposal::ProposalStatus;

use crate::cli::Proposal;

/// What the stop key asks on every tab.
pub const STOP_PROMPT: &str = "Stop every worker now? They continue only after start.";
/// The modes in the order `m` moves through them.
const MODES: [&str; 3] = ["off", "propose", "auto"];
const NO_VIEW: &str = "this coordinator has no dispatcher view; update it to use this tab";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Workers,
    Launch,
    Needs,
    Review,
    Budget,
    Projects,
    Limits,
    Rules,
}

impl Pane {
    pub const ALL: [Pane; 8] = [
        Pane::Workers,
        Pane::Launch,
        Pane::Needs,
        Pane::Review,
        Pane::Budget,
        Pane::Projects,
        Pane::Limits,
        Pane::Rules,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Pane::Workers => "workers",
            Pane::Launch => "would launch",
            Pane::Needs => "needs you",
            Pane::Review => "review",
            Pane::Budget => "budget",
            Pane::Projects => "projects",
            Pane::Limits => "limits",
            Pane::Rules => "rules",
        }
    }

    pub fn index(self) -> usize {
        Pane::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    fn next(self) -> Pane {
        Pane::ALL[(self.index() + 1) % Pane::ALL.len()]
    }
}

/// The limit being typed in the footer.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitInput {
    pub key: String,
    pub text: String,
}

/// The proposals still waiting for a decision, in the order the coordinator lists them.
pub fn open_proposals(d: &Snapshot) -> Vec<&Proposal> {
    d.proposals
        .iter()
        .filter(|p| p.status == ProposalStatus::Open)
        .collect()
}

/// Rows of `pane`: what the selection moves over.
pub fn pane_rows(d: &Snapshot, pane: Pane) -> usize {
    let Some(v) = &d.dispatcher else { return 0 };
    match pane {
        Pane::Workers => v.workers.len(),
        Pane::Launch => v.planned.len() + v.ready_unjudged.len(),
        Pane::Needs => v.needs_you.len(),
        Pane::Review => v.review.len(),
        Pane::Budget => v.budgets.iter().map(|b| b.days.len()).sum(),
        Pane::Projects => v.projects.len(),
        Pane::Limits => d.limits.len() + v.devices.iter().map(|x| x.limits.len()).sum::<usize>(),
        Pane::Rules => open_proposals(d).len(),
    }
}

impl App {
    /// The count in the tab title: the workers, whichever pane shows.
    pub fn tab_count(&self, tab: Tab) -> usize {
        match (tab, self.view()) {
            (Tab::Dispatch, Some(v)) => v.workers.len(),
            (Tab::Dispatch, None) => 0,
            _ => self.rows(tab),
        }
    }

    pub fn view(&self) -> Option<&DispatcherView> {
        self.data.as_ref()?.dispatcher.as_ref()
    }

    /// Keys of the limit input, which comes before every other key: Esc closes it, Enter
    /// sends it. The word `default` puts the limit back to its default; an empty value sends
    /// nothing, so a slip of Enter never relaxes a safety line.
    pub(super) fn input_key(&mut self, key: KeyEvent) -> Option<Request> {
        let input = self.input.as_mut()?;
        match key.code {
            KeyCode::Char(c) => input.text.push(c),
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => {
                let LimitInput { key, text } = self.input.take()?;
                let value = match text.trim() {
                    "" => return None,
                    "default" => None,
                    v => Some(v.to_string()),
                };
                return Some(Request::SetLimit { key, value });
            }
            _ => {}
        }
        None
    }

    /// The Dispatch tab's own keys; None leaves the key to the other tabs' handling.
    pub(super) fn dispatch_key(
        &mut self,
        code: KeyCode,
        now_ms: UnixMs,
    ) -> Option<Option<Request>> {
        if self.tab != Tab::Dispatch {
            return None;
        }
        let KeyCode::Char(c) = code else { return None };
        if self.view().is_none() {
            // The stop key is on every tab, and its route is not the view's.
            if c == 'q' || c == 'x' || c.is_ascii_digit() {
                return None;
            }
            self.flash(NO_VIEW.into(), true, now_ms);
            return Some(None);
        }
        match c {
            'v' => {
                self.pane = self.pane.next();
                self.tables[Tab::Dispatch.index()].select(Some(0));
            }
            'm' => return Some(self.next_mode()),
            'g' => self.ask(
                "Start the stopped workers again? They continue where they were.".into(),
                Request::StartWorkers,
            ),
            't' => return Some(self.switch_project(now_ms)),
            'e' => self.edit_limit(now_ms),
            'a' | 'd' => return Some(self.decide_rule(c == 'a', now_ms)),
            _ => return None,
        }
        Some(None)
    }

    fn selected_row(&self) -> Option<usize> {
        self.selected(Tab::Dispatch)
    }

    fn next_mode(&mut self) -> Option<Request> {
        let now = self.view()?.mode.as_str();
        let at = MODES.iter().position(|m| *m == now).unwrap_or(0);
        let next = MODES[(at + 1) % MODES.len()];
        let request = Request::SetMode(next.into());
        match next {
            "off" => return Some(request),
            "auto" => self.ask(
                "Switch the dispatcher to auto? It then launches workers by itself, inside the \
                 projects, limits and budgets you set."
                    .into(),
                request,
            ),
            _ => self.ask(
                format!("Switch the dispatcher to {next}? It shows what it would launch."),
                request,
            ),
        }
        None
    }

    fn switch_project(&mut self, now_ms: UnixMs) -> Option<Request> {
        let p = (self.pane == Pane::Projects)
            .then(|| self.selected_row())
            .flatten()
            .and_then(|i| self.view()?.projects.get(i));
        let Some(p) = p else {
            self.flash(
                "t: select a project on the projects pane (v)".into(),
                true,
                now_ms,
            );
            return None;
        };
        let on = !p.enabled;
        let request = Request::SwitchProject {
            id: p.id,
            name: p.name.clone(),
            on,
        };
        if !on {
            return Some(request);
        }
        let prompt = format!(
            "Switch project {} on? The dispatcher may then plan and launch workers on it.",
            p.name
        );
        self.ask(prompt, request);
        None
    }

    fn edit_limit(&mut self, now_ms: UnixMs) {
        let own = self.data.as_ref().map_or(&[][..], |d| &d.limits[..]);
        let limit = (self.pane == Pane::Limits)
            .then(|| self.selected_row())
            .flatten()
            .map(|i| own.get(i).ok_or(i));
        match limit {
            Some(Ok(s)) => {
                self.input = Some(LimitInput {
                    key: s.key.clone(),
                    text: String::new(),
                })
            }
            Some(Err(_)) => self.flash(
                "read-only: change it on that device with idlemax config set".into(),
                true,
                now_ms,
            ),
            None => self.flash(
                "e: select a limit on the limits pane (v)".into(),
                true,
                now_ms,
            ),
        }
    }

    fn decide_rule(&mut self, approve: bool, now_ms: UnixMs) -> Option<Request> {
        let picked = (self.pane == Pane::Rules)
            .then(|| self.selected_row())
            .flatten()
            .and_then(|i| {
                open_proposals(self.data.as_ref()?)
                    .get(i)
                    .map(|p| (p.id, p.line.clone()))
            });
        let Some((id, line)) = picked else {
            self.flash(
                "a/d: select a proposal on the rules pane (v)".into(),
                true,
                now_ms,
            );
            return None;
        };
        if !approve {
            return Some(Request::RejectRule(id));
        }
        self.ask(
            format!("Add this line to rules.md? {line}"),
            Request::ApproveRule(id),
        );
        None
    }
}
