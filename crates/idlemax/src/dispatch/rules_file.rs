//! Adapter: `$IDLEMAX_HOME/rules.md`, the orchestrator's judgment rules (D18.2). Created once
//! with the defaults and never overwritten; read at pass time, so an edit applies to the next
//! pass with no restart.

use std::io::Write;

use idlemax_core::dispatch::prompt::DEFAULT_RULES;
use idlemax_core::Paths;

use crate::state;

/// Longest rules text a pass reads.
pub const MAX_RULES_BYTES: usize = 16 * 1024;

/// Writes the default rules when the file does not exist yet.
pub fn ensure(paths: &Paths) -> std::io::Result<()> {
    let mut opts = state::private_file();
    match opts.write(true).create_new(true).open(paths.rules_file()) {
        Ok(mut file) => file.write_all(DEFAULT_RULES.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// The rules text now, clipped to 16 KiB, or the defaults when the file is missing.
pub fn read(paths: &Paths) -> String {
    match std::fs::read_to_string(paths.rules_file()) {
        Ok(text) => {
            let mut end = text.len().min(MAX_RULES_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_string()
        }
        Err(_) => DEFAULT_RULES.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_file_is_written_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        assert_eq!(read(&paths), DEFAULT_RULES);
        ensure(&paths).unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.rules_file()).unwrap(),
            DEFAULT_RULES
        );
        std::fs::write(paths.rules_file(), "# mine\n").unwrap();
        ensure(&paths).unwrap();
        assert_eq!(std::fs::read(paths.rules_file()).unwrap(), b"# mine\n");
        assert_eq!(read(&paths), "# mine\n");
    }
}
