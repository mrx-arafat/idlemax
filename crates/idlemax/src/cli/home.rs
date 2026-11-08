//! Delivery: `idlemax` with no arguments and the session-hook summary. One status call and one
//! quota call, then one render; a coordinator that is down gives a short note, never an error.

mod render;
#[cfg(test)]
mod tests;

use std::time::Duration;

use anyhow::Result;
use idlemax_core::DEFAULT_COORD_URL;

use super::{ApiClient, ClientError, Format, Out};
use crate::state::{self, StateError};
pub use render::{collapse_home, Mode, COMMANDS, DESCRIPTION};
use render::{Home, Reason, State};

/// A session start must not wait on a coordinator that does not answer.
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// Prints the home view or, in hook mode, the compact session summary.
pub async fn run(coord: Option<String>, out: &Out, mode: Mode) -> Result<()> {
    let home = gather(coord).await;
    if out.format() == Format::Json && mode == Mode::Full {
        return out.emit(&home, |h| render::render(h, mode), &[]);
    }
    out.raw(&render::render(&home, mode))
}

async fn gather(coord: Option<String>) -> Home {
    let (url, client) = connect(coord);
    let state = match client {
        Err(down) => down,
        Ok(client) => {
            let both = async { tokio::try_join!(client.status(), client.quota()) };
            match tokio::time::timeout(FETCH_TIMEOUT, both).await {
                Ok(Ok((status, quota))) => State::Up(render::live(&status, &quota)),
                Ok(Err(ClientError::Unreachable { .. })) | Err(_) => {
                    down(Reason::Unreachable, String::new())
                }
                Ok(Err(e)) => down(Reason::Failed, e.describe()),
            }
        }
    };
    Home {
        bin: bin_display(),
        description: DESCRIPTION,
        coordinator: url,
        state,
    }
}

fn down(reason: Reason, detail: String) -> State {
    State::Down { reason, detail }
}

/// The coordinator URL and a client for it, or the reason there is none.
fn connect(coord: Option<String>) -> (String, Result<ApiClient, State>) {
    let fallback = || {
        coord
            .clone()
            .unwrap_or_else(|| DEFAULT_COORD_URL.to_string())
    };
    let failed = |e: &dyn std::fmt::Display| down(Reason::Failed, e.to_string());
    let paths = match state::paths_from_env() {
        Ok(p) => p,
        Err(e) => return (fallback(), Err(failed(&e))),
    };
    let config = match state::load_config(&paths) {
        Ok(c) => c,
        Err(e) => return (fallback(), Err(failed(&e))),
    };
    let url = coord.unwrap_or(config.coordinator.url);
    let token = match state::load_token(&paths) {
        Ok(t) => t,
        Err(StateError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return (url, Err(down(Reason::NotSetUp, String::new())));
        }
        Err(e) => return (url, Err(failed(&e))),
    };
    let client = ApiClient::new(&url, token).map_err(|e| failed(&e));
    (url, client)
}

/// This executable's path with the home directory shown as `~`.
fn bin_display() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "idlemax".into());
    render::collapse_home(&exe, dirs::home_dir().as_deref())
}
