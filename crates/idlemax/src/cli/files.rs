//! Adapter: replace a file in one step so a crash never leaves it half written, and keep a
//! backup of what the user had before idlemax edited their config files.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Writes `bytes` to a sibling temp file, then renames it over `path`.
/// `mode` sets the permissions; `None` keeps the existing file's and otherwise uses the umask.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".idlemax-tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = write_then_rename(&tmp, path, bytes, mode);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `<file>.idlemax.bak`: the file as it was before the last change idlemax made.
/// It sits next to the file the path really points to, so a symlinked settings.json works.
pub fn backup_path(settings: &Path) -> PathBuf {
    let target = std::fs::canonicalize(settings).unwrap_or_else(|_| settings.to_path_buf());
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".idlemax.bak");
    target.with_file_name(name)
}

pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes `after` over `settings` (following a symlink) after copying the old file aside.
pub fn save(settings: &Path, before: Option<&str>, after: &str) -> Result<Option<PathBuf>> {
    if let Some(dir) = settings.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let target = std::fs::canonicalize(settings).unwrap_or_else(|_| settings.to_path_buf());
    let backup = before.map(|_| backup_path(&target));
    if let (Some(path), Some(old)) = (&backup, before) {
        write_atomic(path, old.as_bytes(), None)
            .with_context(|| format!("backing up to {}", path.display()))?;
    }
    write_atomic(&target, after.as_bytes(), None)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(backup)
}

fn write_then_rename(tmp: &Path, path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    std::fs::write(tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = match (mode, std::fs::metadata(path)) {
            (Some(m), _) => Some(std::fs::Permissions::from_mode(m)),
            (None, Ok(existing)) => Some(existing.permissions()),
            (None, Err(_)) => None,
        };
        if let Some(perms) = perms {
            std::fs::set_permissions(tmp, perms)?;
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(tmp, path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replaces_content_and_keeps_or_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&file, b"new", None).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_atomic(&file, b"x", Some(0o755)).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp file left"
        );
    }
}
