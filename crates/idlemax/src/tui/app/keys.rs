//! Delivery: keys to actions: tab switching, selection, and the keys that ask for a request.

use idlemax_core::types::{ControlAction, ControlScope, UnixMs};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::requests::control;
use super::{App, Request, Tab};

/// Lines PgUp and PgDn move the explanation.
const WHY_SCROLL: isize = 5;

impl App {
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

    pub(super) fn switch(&mut self, step: usize) {
        self.tab = Tab::ALL[(self.tab.index() + step) % Tab::ALL.len()];
    }
}
