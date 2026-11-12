//! Adapter: the newest `config.toml` that parsed, read again when the file changes, so both
//! daemons see a changed `[dispatcher]` section without a restart (A2).
//!
//! `current` does one blocking `metadata` call, and a read when the file changed: call it
//! inside `with_store` on the coordinator and inside `spawn_blocking` on the agent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use idlemax_core::{Config, Paths};

use crate::state;

/// What identifies one version of the file: its mtime and its length.
type Stamp = Option<(SystemTime, u64)>;

pub struct LiveConfig {
    paths: Paths,
    file: PathBuf,
    held: Mutex<(Stamp, Arc<Config>)>,
}

impl LiveConfig {
    /// `start` is the config the process started with; it holds until the file changes.
    pub fn new(paths: &Paths, start: Config) -> Self {
        let file = paths.config_file();
        let stamp = stamp(&file);
        Self {
            paths: paths.clone(),
            file,
            held: Mutex::new((stamp, Arc::new(start))),
        }
    }

    /// The newest config that parsed. A file that does not parse keeps the last good copy and
    /// logs one warning per change; a missing file keeps the copy it has.
    pub fn current(&self) -> Arc<Config> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let now = stamp(&self.file);
        if now.is_none() || now == held.0 {
            return Arc::clone(&held.1);
        }
        held.0 = now;
        match state::load_config(&self.paths) {
            Ok(cfg) => held.1 = Arc::new(cfg),
            Err(e) => {
                tracing::warn!(error = %e, "config.toml changed but does not parse; keeping the last good copy")
            }
        }
        Arc::clone(&held.1)
    }
}

fn stamp(file: &std::path::Path) -> Stamp {
    let meta = std::fs::metadata(file).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use idlemax_core::dispatch::Mode;

    use super::*;

    /// Writes `text` and moves the mtime forward by `secs`, so the test does not depend on
    /// the clock's resolution.
    fn write(file: &std::path::Path, text: &str, secs: u64) {
        std::fs::write(file, text).unwrap();
        let f = std::fs::File::options().write(true).open(file).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs))
            .unwrap();
    }

    #[test]
    fn live_config_follows_the_file_and_keeps_the_last_good_copy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let file = paths.config_file();
        write(&file, "", 1);
        let live = LiveConfig::new(&paths, Config::default());
        assert_eq!(live.current().dispatcher.mode, Mode::Propose);

        write(
            &file,
            "[dispatcher]\nmode = \"off\"\nhold_mem_used_pct = 80\n",
            2,
        );
        let cfg = live.current();
        assert_eq!(cfg.dispatcher.mode, Mode::Off);
        assert_eq!(cfg.dispatcher.hold_mem_used_pct, 80.0);

        write(&file, "[dispatcher\nbroken", 3);
        assert_eq!(live.current().dispatcher.mode, Mode::Off);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(live.current().dispatcher.hold_mem_used_pct, 80.0);
    }
}
