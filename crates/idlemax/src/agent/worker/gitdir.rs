//! Adapter: the git config and hooks a project repository shares with every worktree. A
//! snapshot taken before a launch (a content hash, and a copy under the agent's state dir)
//! lets the end of the worker find any change to them and put them back, so a worker cannot
//! leave code there for the owner's git or the agent's to run (red-team case 5).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::cmd::{Cmd, Commands};

const GIT_LIMIT: Duration = Duration::from_secs(10);
/// What is snapshotted, relative to the common dir.
const WATCHED: [&str; 2] = ["config", "hooks"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSnapshot {
    /// The repository's common git dir (`git rev-parse --git-common-dir`).
    pub common: PathBuf,
    /// Where the copy of `config` and `hooks/` is kept.
    pub copy: PathBuf,
    /// sha256 over both, as `digest` reads them.
    pub hash: String,
}

/// Copies `config` and `hooks/` of the repository that holds `dir` into `copy`, replacing
/// whatever an older snapshot left there.
pub fn take(commands: &dyn Commands, dir: &Path, copy: &Path) -> Result<GitSnapshot, String> {
    let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    let out = commands
        .run(&Cmd::git(dir, &args, GIT_LIMIT))
        .map_err(|e| format!("git rev-parse: {e}"))?;
    if out.code != Some(0) {
        return Err(format!("git rev-parse: {}", out.stderr.trim()));
    }
    let common = PathBuf::from(out.stdout.trim());
    if !common.is_absolute() {
        return Err(format!(
            "git rev-parse gave no absolute git dir for {}",
            dir.display()
        ));
    }
    let copied = remove_any(copy)
        .and_then(|()| crate::state::create_private_dir(copy))
        .and_then(|()| put_back(&common, copy));
    let hash = copied.and_then(|()| digest(&common)).map_err(|e| {
        format!(
            "the git config and hooks of {} could not be copied: {e}",
            common.display()
        )
    })?;
    Ok(GitSnapshot {
        common,
        copy: copy.to_path_buf(),
        hash,
    })
}

/// True when `config` or `hooks/` differ from the snapshot; they are then put back as they
/// were. A change that cannot be read or undone still counts as a change.
pub fn undo_change(s: &GitSnapshot) -> bool {
    if digest(&s.common).is_ok_and(|h| h == s.hash) {
        return false;
    }
    let restored = put_back(&s.copy, &s.common).and_then(|()| digest(&s.common));
    match restored {
        Ok(h) if h == s.hash => {
            tracing::warn!(dir = %s.common.display(), "a worker changed the repository's git config or hooks; restored")
        }
        _ => {
            tracing::error!(dir = %s.common.display(), "a worker changed the repository's git config or hooks; restoring them failed")
        }
    }
    true
}

/// Removes the copy once the launch has ended.
pub fn discard(s: &GitSnapshot) {
    let _ = remove_any(&s.copy);
}

/// Replaces `config` and `hooks` in `to` with those in `from`; one missing in `from` is
/// removed from `to`.
fn put_back(from: &Path, to: &Path) -> io::Result<()> {
    for name in WATCHED {
        let target = to.join(name);
        remove_any(&target)?;
        if fs::symlink_metadata(from.join(name)).is_ok() {
            copy_tree(&from.join(name), &target)?;
        }
    }
    Ok(())
}

/// sha256 over each watched path: its kind, mode, link target or contents, and the entries of
/// a folder in name order.
fn digest(common: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    for name in WATCHED {
        feed(&mut hasher, &common.join(name), name)?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn feed(hasher: &mut Sha256, path: &Path, rel: &str) -> io::Result<()> {
    hasher.update(rel.as_bytes());
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            hasher.update(b"\0none\0");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    hasher.update(format!("\0{}\0", mode(&meta)).as_bytes());
    if meta.file_type().is_symlink() {
        hasher.update(b"link\0");
        hasher.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
    } else if meta.is_dir() {
        hasher.update(b"dir\0");
        let mut names: Vec<String> = fs::read_dir(path)?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<_>>()?;
        names.sort();
        for n in names {
            feed(hasher, &path.join(&n), &format!("{rel}/{n}"))?;
        }
    } else {
        hasher.update(format!("file\0{}\0", meta.len()).as_bytes());
        io::copy(&mut fs::File::open(path)?, hasher)?;
    }
    hasher.update(b"\0end\0");
    Ok(())
}

/// Copies a file, a folder or a symlink as it is, with its mode.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        return symlink(&fs::read_link(from)?, to);
    }
    if !meta.is_dir() {
        return fs::copy(from, to).map(|_| ());
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let name = entry?.file_name();
        copy_tree(&from.join(&name), &to.join(&name))?;
    }
    fs::set_permissions(to, meta.permissions())
}

fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode(meta: &fs::Metadata) -> u32 {
    u32::from(meta.permissions().readonly())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "symlinks in .git are not copied on this platform",
    ))
}

#[cfg(test)]
mod tests;
