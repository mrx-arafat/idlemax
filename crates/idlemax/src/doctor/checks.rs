//! Adapter: the state dir, config and token checks, the base every other check builds on.

use std::path::Path;

use idlemax_core::config::{parse_token, token_mode_is_private};
use idlemax_core::doctor::{unknown_config_keys, Check, Report};
use idlemax_core::paths::{HomeError, HOME_ENV};
use idlemax_core::{Config, Paths, TOKEN_ENV};

use crate::cli::rows;
use crate::state::{self, StateError};

/// The fix for a check that could not run because there is no state dir.
pub const AFTER_HOME: &str = "resolve the home check above, then run `idlemax doctor` again";

/// The fix for a check that could not run because there is no usable token.
pub const AFTER_TOKEN: &str = "resolve the token check above, then run `idlemax doctor` again";

/// Check 1a: where the state dir is. `None` means every later check has no home.
pub fn home(report: &mut Report) -> Option<Paths> {
    match state::paths_from_env() {
        Ok(paths) => {
            let root = paths.root().to_string_lossy().into_owned();
            report.set_home(&root);
            let source = match std::env::var_os(HOME_ENV).is_some() {
                true => HOME_ENV.to_string(),
                false => "default".into(),
            };
            report.push(home_check(&root, &source, dir_mode(paths.root())));
            Some(paths)
        }
        Err(StateError::Home(HomeError::Empty)) => {
            report.push(Check::fail(
                "home",
                format!("{HOME_ENV} is set but empty, so there is no state dir"),
                format!(
                    "unset {HOME_ENV}, or set it to the directory holding config.toml and token"
                ),
            ));
            None
        }
        Err(e) => {
            report.push(Check::fail(
                "home",
                e.to_string(),
                format!("set {HOME_ENV} to the state dir, or set HOME so the default ~/.idlemax resolves"),
            ));
            None
        }
    }
}

/// The state dir resolves; a warning when other users can reach it, since job logs, inbox notes
/// and metrics live there. `mode` is its permission bits, None where the platform has none.
pub fn home_check(root: &str, source: &str, mode: Option<u32>) -> Check {
    match mode.filter(|m| !token_mode_is_private(*m)) {
        Some(open) => Check::warn(
            "home",
            format!(
                "{root} ({source}) is mode {open:04o}: other users can read job logs and notes"
            ),
            format!("chmod 700 {root}"),
        ),
        None => Check::ok("home", format!("{root} ({source})")),
    }
}

#[cfg(unix)]
fn dir_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn dir_mode(_path: &Path) -> Option<u32> {
    None
}

/// Check 1b: `config.toml` parses, and every key in it is one the parser uses.
pub fn config(report: &mut Report, paths: Option<&Paths>) -> Config {
    let Some(paths) = paths else {
        report.push(Check::warn(
            "config",
            "not checked: no state dir",
            AFTER_HOME,
        ));
        return Config::default();
    };
    let path = paths.config_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.push(Check::warn(
                "config",
                format!("no {}: built-in defaults are in effect", path.display()),
                "run `idlemax init` to write a config.toml that lists every key",
            ));
            return Config::default();
        }
        Err(e) => {
            report.push(Check::fail(
                "config",
                format!("cannot read {}: {e}", path.display()),
                "fix the file permissions, or move it aside and run `idlemax init`",
            ));
            return Config::default();
        }
    };
    match Config::parse(&text) {
        Ok(cfg) => {
            report.push(keys_check(&path, &text));
            cfg
        }
        Err(e) => {
            report.push(Check::fail(
                "config",
                format!("{}: {}", path.display(), rows::trunc(&one_line(&e.to_string()), 200)),
                "fix the key or value named above; `idlemax init` in an empty directory writes a complete example",
            ));
            Config::default()
        }
    }
}

/// The config parsed; unknown keys are ignored by the parser, so a typo needs a warning.
fn keys_check(path: &Path, text: &str) -> Check {
    match unknown_config_keys(text) {
        Ok(unknown) if !unknown.is_empty() => Check::warn(
            "config",
            format!("unknown keys in {}: {}", path.display(), unknown.join(", ")),
            "remove or fix them: idlemax ignores unknown keys, so a typo silently does nothing",
        ),
        // Parsing twice cannot disagree, so an error here still means a good config.
        _ => Check::ok("config", format!("{} parses", path.display())),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Check 2: the bearer token is there and only the owner can read it.
pub fn token(report: &mut Report, paths: Option<&Paths>) -> Option<String> {
    if let Some(value) = std::env::var_os(TOKEN_ENV) {
        return match parse_token(&value.to_string_lossy()) {
            Ok(token) => {
                report.push(Check::ok(
                    "token",
                    format!("{TOKEN_ENV} is set (the token file is not used)"),
                ));
                Some(token)
            }
            Err(_) => {
                report.push(Check::fail(
                    "token",
                    format!("{TOKEN_ENV} is set but empty"),
                    format!("unset {TOKEN_ENV}, or set it to the shared coordinator token"),
                ));
                None
            }
        };
    }
    let Some(paths) = paths else {
        report.push(Check::warn(
            "token",
            "not checked: no state dir",
            AFTER_HOME,
        ));
        return None;
    };
    token_file(report, &paths.token_file())
}

/// The file side of check 2, without the environment override.
pub fn token_file(report: &mut Report, path: &Path) -> Option<String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            report.push(stat_failure(path, &e));
            return None;
        }
    };
    let mode = mode_of(&meta);
    if !token_mode_is_private(mode) {
        report.push(Check::fail(
            "token",
            format!(
                "{} is mode {:04o}: group and others can read it",
                path.display(),
                mode & 0o777
            ),
            format!("chmod 600 {}", path.display()),
        ));
        return None;
    }
    match std::fs::read_to_string(path).map(|text| parse_token(&text)) {
        Ok(Ok(token)) => {
            report.push(Check::ok(
                "token",
                format!(
                    "{} is mode {:04o}, {} chars",
                    path.display(),
                    mode & 0o777,
                    token.len()
                ),
            ));
            Some(token)
        }
        Ok(Err(_)) => {
            report.push(Check::fail(
                "token",
                format!("{} is empty", path.display()),
                format!("write the shared token to {}, or remove it and run `idlemax init --token-stdin`", path.display()),
            ));
            None
        }
        Err(e) => {
            report.push(Check::fail(
                "token",
                format!("cannot read {}: {e}", path.display()),
                "fix the file permissions, or remove the file and run `idlemax init --token-stdin`",
            ));
            None
        }
    }
}

fn stat_failure(path: &Path, e: &std::io::Error) -> Check {
    match e.kind() {
        std::io::ErrorKind::NotFound => Check::fail(
            "token",
            format!("no token file at {}", path.display()),
            "run `idlemax init` on the first device; on another device pipe the shared token into `idlemax init --token-stdin`",
        ),
        _ => Check::fail(
            "token",
            format!("cannot stat {}: {e}", path.display()),
            "fix the file permissions, or remove the file and run `idlemax init --token-stdin`",
        ),
    }
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o600
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_a_toml_error_to_the_line_number() {
        let text =
            "TOML parse error at line 3, column 1\n  |\n3 | [agent\n  | ^\ninvalid table header\n";
        assert_eq!(
            one_line(text),
            "TOML parse error at line 3, column 1 | 3 | [agent | ^ invalid table header"
        );
    }
}
