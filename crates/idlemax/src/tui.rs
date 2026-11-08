//! Delivery: ratatui dashboard. Owns the terminal and the event loop; data comes from `feed`.

mod app;
mod feed;
mod layout;
#[cfg(test)]
mod snapshots;
mod table;
mod view;

use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

use ratatui::crossterm::event;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedSender;

use crate::cli::ApiClient;
use crate::state::now_ms;
use app::App;
use feed::{Request, Update};

/// Input poll interval; also the redraw cadence while idle.
const TICK: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("the dashboard needs an interactive terminal")]
    NotATerminal,
    #[error("terminal: {0}")]
    Terminal(#[from] std::io::Error),
    #[error("dashboard thread: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("background fetch task stopped")]
    FeedStopped,
}

pub async fn run(client: ApiClient) -> Result<(), TuiError> {
    if !std::io::stdout().is_terminal() {
        return Err(TuiError::NotATerminal);
    }
    let source = client.base().to_string();
    let (req_tx, req_rx) = tokio::sync::mpsc::unbounded_channel();
    let (upd_tx, upd_rx) = mpsc::channel();
    let feed = tokio::spawn(feed::run(client, req_rx, upd_tx));
    // Terminal I/O blocks, so the UI loop lives on a blocking thread.
    let ui = tokio::task::spawn_blocking(move || ui_thread(source, &req_tx, &upd_rx)).await;
    feed.abort();
    ui?
}

fn ui_thread(
    source: String,
    requests: &UnboundedSender<Request>,
    updates: &mpsc::Receiver<Update>,
) -> Result<(), TuiError> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, App::new(source), requests, updates);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    requests: &UnboundedSender<Request>,
    updates: &mpsc::Receiver<Update>,
) -> Result<(), TuiError> {
    loop {
        loop {
            match updates.try_recv() {
                Ok(u) => app.apply(u, now_ms()),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Err(TuiError::FeedStopped),
            }
        }
        terminal.draw(|f| view::render(f, &mut app, now_ms()))?;
        if app.quit {
            return Ok(());
        }
        if !event::poll(TICK)? {
            continue;
        }
        let Some(key) = event::read()?.as_key_press_event() else {
            continue;
        };
        if let Some(req) = app.on_key(key, now_ms()) {
            requests.send(req).map_err(|_| TuiError::FeedStopped)?;
        }
    }
}
