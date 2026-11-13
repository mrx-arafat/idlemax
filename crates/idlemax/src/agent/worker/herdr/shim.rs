//! Adapter: puts the sandbox shim first on a Herdr pane's `PATH` again after the shell's
//! start-up files ran, since a `.bashrc` that prepends `~/.local/bin` would hide it (A13).

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

/// One shell word in single quotes.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}
