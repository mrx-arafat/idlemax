//! Adapter: reads the environment, the state dir files and the clock for the domain.
//! Owned by im-contract.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use idlemax_core::config::{encode_token, parse_token, token_mode_is_private, ConfigError};
use idlemax_core::paths::{resolve_home, HomeError, HOME_ENV};
use idlemax_core::types::UnixMs;
use idlemax_core::{Config, Paths, TOKEN_ENV};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Home(#[from] HomeError),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Config {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    #[error("{} must be mode 0600", .0.display())]
    TokenNotPrivate(PathBuf),
    #[error("{TOKEN_ENV} is set but empty")]
    EmptyTokenEnv,
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> StateError + '_ {
    move |source| StateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Wall clock in Unix milliseconds.
pub fn now_ms() -> UnixMs {
    chrono::Utc::now().timestamp_millis()
}

/// State dir from `IDLEMAX_HOME`, else `<user home>/.idlemax`.
pub fn paths_from_env() -> Result<Paths, StateError> {
    Ok(Paths::new(resolve_home(
        std::env::var_os(HOME_ENV),
        user_home_var(),
    )?))
}

#[cfg(windows)]
fn user_home_var() -> Option<OsString> {
    std::env::var_os("USERPROFILE")
}

#[cfg(not(windows))]
fn user_home_var() -> Option<OsString> {
    std::env::var_os("HOME")
}

pub fn ensure_dirs(paths: &Paths) -> Result<(), StateError> {
    for dir in paths.dirs() {
        std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    }
    Ok(())
}

/// Missing `config.toml` gives defaults; a malformed one is an error.
pub fn load_config(paths: &Paths) -> Result<Config, StateError> {
    let path = paths.config_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text).map_err(|source| StateError::Config { path, source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(io_err(&path)(e)),
    }
}

/// Bearer token: `IDLEMAX_TOKEN` if set, else the 0600 token file.
pub fn load_token(paths: &Paths) -> Result<String, StateError> {
    if let Some(v) = std::env::var_os(TOKEN_ENV) {
        return parse_token(&v.to_string_lossy()).map_err(|_| StateError::EmptyTokenEnv);
    }
    read_token_file(&paths.token_file())
}

fn read_token_file(path: &Path) -> Result<String, StateError> {
    if !token_mode_is_private(file_mode(path)?) {
        return Err(StateError::TokenNotPrivate(path.to_path_buf()));
    }
    let text = std::fs::read_to_string(path).map_err(io_err(path))?;
    parse_token(&text).map_err(|source| StateError::Config {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<u32, StateError> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)
        .map_err(io_err(path))?
        .permissions()
        .mode())
}

// SHORTCUT: no ACL check on Windows, add one when a Windows device joins.
#[cfg(not(unix))]
fn file_mode(path: &Path) -> Result<u32, StateError> {
    std::fs::metadata(path).map_err(io_err(path))?;
    Ok(0o600)
}

/// Writes the token file with mode 0600. Fails if it already exists.
pub fn write_token(paths: &Paths, token: &str) -> Result<(), StateError> {
    let path = paths.token_file();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).map_err(io_err(&path))?;
    writeln!(f, "{token}").map_err(io_err(&path))
}

// SHORTCUT: /dev/urandom only, use BCryptGenRandom when a Windows device joins.
/// 32 random bytes from the OS RNG as hex.
pub fn generate_token() -> Result<String, StateError> {
    let path = Path::new("/dev/urandom");
    let mut buf = [0u8; 32];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .map_err(io_err(path))?;
    Ok(encode_token(&buf))
}
