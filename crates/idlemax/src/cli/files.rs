//! Adapter: replace a file in one step so a crash never leaves it half written.

use std::io;
use std::path::Path;

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
