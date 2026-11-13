//! Adapter: puts the sandbox shim first on a Herdr pane's `PATH` again after the shell's
//! start-up files ran, since a `.bashrc` that prepends `~/.local/bin` would hide it (A13),
//! and starts the agent once Herdr calls the pane free again.

use std::time::Duration;

use super::{strings, HerdrHost};
use crate::agent::worker::host::{HostError, Launch};
use crate::agent::worker::sandbox::SHIM_DIR_VAR;

/// Printed by the pane once the export ran; the typed line shows `$((40+2))` instead.
const DONE: &str = "idlemax-shim-42";

impl HerdrHost {
    /// With a shim in `launch`, types the export into the pane and waits for it to finish.
    pub(super) fn shim_first(&self, pane: &str, launch: &Launch) -> Result<(), HostError> {
        let Some((_, dir)) = launch.env.iter().find(|(k, _)| k == SHIM_DIR_VAR) else {
            return Ok(());
        };
        let line = format!(
            "export PATH={}:\"$PATH\"; hash -r; echo idlemax-shim-$((40+2))",
            quote(dir)
        );
        self.call(&strings(&["pane", "run", pane, &line]))?;
        self.call(&strings(&[
            "pane",
            "wait-output",
            "--match",
            DONE,
            "--timeout",
            "10000",
            pane,
        ]))
        .map(drop)
    }
}

impl HerdrHost {
    /// `agent start`, tried again for up to 5 s while Herdr calls the pane busy: a new pane
    /// that just ran the export is not an available shell for about half a second (live).
    pub(super) fn start_when_free(&self, args: &[String]) -> Result<String, HostError> {
        for _ in 0..10 {
            match self.call(args) {
                Err(HostError::Failed { why, .. }) if why.contains("agent_pane_busy") => {
                    std::thread::sleep(Duration::from_millis(500));
                }
                other => return other,
            }
        }
        self.call(args)
    }
}

/// One shell word in single quotes.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}
