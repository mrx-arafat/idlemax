//! Adapter: the owner-writes ledger and the digests of idlemax's own files in the state dir
//! (red team cases 2 and 3). An owner command that writes `config.toml` or `rules.md` adds
//! the new digest to the ledger; the agent compares a worker's start and end digests through
//! the pure `worker::tamper`.

use std::io::Write;
use std::path::Path;

use idlemax_core::worker::tamper::{digest, ledger_line, tampered, ABSENT, GUARDED, LEDGER};

/// Records that an owner command wrote `file` with `content`. A failure is logged only: a
/// missing line can only make a worker's turn look like it changed the file.
pub fn record(file: &Path, content: &str) {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    if !GUARDED.contains(&name) {
        return;
    }
    let line = ledger_line(name, &digest(content.as_bytes()));
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LEDGER));
    if let Err(e) = opened.and_then(|mut f| f.write_all(line.as_bytes())) {
        tracing::warn!(file = %file.display(), error = %e, "the owner write was not recorded");
    }
}

/// The digest of each guarded file in `state_dir`, `absent` for one that does not exist.
pub fn snapshot(state_dir: &Path) -> Vec<(String, String)> {
    GUARDED
        .iter()
        .map(|name| {
            let d =
                std::fs::read(state_dir.join(name)).map_or_else(|_| ABSENT.into(), |b| digest(&b));
            (name.to_string(), d)
        })
        .collect()
}

/// The guarded files changed since `before` without an owner command recording them.
pub fn tampered_since(state_dir: &Path, before: &[(String, String)]) -> Vec<String> {
    let ledger = std::fs::read_to_string(state_dir.join(LEDGER)).unwrap_or_default();
    tampered(before, &snapshot(state_dir), &ledger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_an_owner_command_recorded_is_fine_and_any_other_is_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        std::fs::write(
            state.join("config.toml"),
            "[dispatcher]\nmode = \"propose\"\n",
        )
        .unwrap();
        let before = snapshot(state);
        // `idlemax mode auto` through the owner route: saved, then recorded.
        let owner = "[dispatcher]\nmode = \"auto\"\n";
        std::fs::write(state.join("config.toml"), owner).unwrap();
        record(&state.join("config.toml"), owner);
        assert!(tampered_since(state, &before).is_empty());
        // The worker appends a line itself, and writes a token where there was none.
        std::fs::write(
            state.join("config.toml"),
            "[dispatcher]\nmode = \"auto\"\n#x\n",
        )
        .unwrap();
        std::fs::write(state.join("token"), "stolen").unwrap();
        assert_eq!(tampered_since(state, &before), ["config.toml", "token"]);
    }
}
