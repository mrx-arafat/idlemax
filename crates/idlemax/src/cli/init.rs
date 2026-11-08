//! Delivery: `idlemax init` creates the state dir, `config.toml` and the token, never overwriting.

use std::io::Write;

use anyhow::{bail, Context, Result};
use idlemax_core::types::{DeviceProfile, Os};
use idlemax_core::{Config, Paths};

use crate::state;

#[derive(Debug, Default)]
pub struct InitOptions {
    pub profile: Option<DeviceProfile>,
    pub coord_url: Option<String>,
    /// Shared coordinator token; None generates a new one.
    pub token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Kept,
}

#[derive(Debug)]
pub struct InitReport {
    pub config: Outcome,
    pub token: Outcome,
}

pub fn init(paths: &Paths, opts: &InitOptions) -> Result<InitReport> {
    if let Some(token) = &opts.token {
        idlemax_core::config::parse_token(token)?;
    }
    if let Some(url) = &opts.coord_url {
        let valid = reqwest::Url::parse(url.trim())
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
        if !valid {
            bail!("--coord-url must be an http:// or https:// URL with a host");
        }
    }
    state::ensure_dirs(paths)?;
    let config = write_config(paths, opts)?;
    let token = write_token_file(paths, opts.token.as_deref())?;
    Ok(InitReport { config, token })
}

fn write_config(paths: &Paths, opts: &InitOptions) -> Result<Outcome> {
    let path = paths.config_file();
    if path.exists() {
        if opts.profile.is_some() || opts.coord_url.is_some() {
            bail!(
                "{} already exists; edit it instead of passing flags",
                path.display()
            );
        }
        state::load_config(paths)?;
        return Ok(Outcome::Kept);
    }
    let mut cfg = Config::default();
    cfg.agent.profile = Some(
        opts.profile
            .unwrap_or_else(|| DeviceProfile::default_for(Os::current())),
    );
    if let Some(url) = &opts.coord_url {
        cfg.coordinator.url = url.trim().trim_end_matches('/').to_string();
    }
    write_new_config(paths, &cfg)?;
    Ok(Outcome::Created)
}

/// Writes `cfg` as `config.toml`, which must not exist yet.
pub(super) fn write_new_config(paths: &Paths, cfg: &Config) -> Result<()> {
    cfg.validate()?;
    let path = paths.config_file();
    let mut f = state::private_file()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(cfg.to_toml_string()?.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

pub(super) fn write_token_file(paths: &Paths, given: Option<&str>) -> Result<Outcome> {
    let path = paths.token_file();
    if path.exists() {
        if given.is_some() {
            bail!(
                "{} already exists; remove it first to replace the token",
                path.display()
            );
        }
        return Ok(Outcome::Kept);
    }
    let token = match given {
        Some(t) => idlemax_core::config::parse_token(t)?,
        None => state::generate_token()?,
    };
    state::write_token(paths, &token)?;
    Ok(Outcome::Created)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_shared_token_that_is_too_short_is_refused_and_nothing_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("state"));
        let opts = InitOptions {
            token: Some("a".into()),
            coord_url: Some("http://127.0.0.1:18101".into()),
            ..InitOptions::default()
        };
        let err = init(&paths, &opts).unwrap_err().to_string();
        assert!(err.contains("at least 32"), "{err}");
        assert!(!paths.token_file().exists());
        assert!(!paths.config_file().exists());
        let retry = InitOptions {
            token: Some("0123456789abcdef0123456789abcdef".into()),
            ..opts
        };
        init(&paths, &retry).unwrap();
        assert_eq!(
            state::load_config(&paths).unwrap().coordinator.url,
            "http://127.0.0.1:18101"
        );
    }

    #[test]
    fn init_rejects_invalid_coordinator_urls_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("state"));
        for url in ["127.0.0.1:18101", "file:///tmp/coord", "http://"] {
            let opts = InitOptions {
                coord_url: Some(url.into()),
                ..InitOptions::default()
            };
            let err = init(&paths, &opts).unwrap_err().to_string();
            assert!(err.contains("--coord-url"), "{err}");
            assert!(!paths.config_file().exists());
            assert!(!paths.token_file().exists());
        }
        let opts = InitOptions {
            coord_url: Some(" http://127.0.0.1:18101/ ".into()),
            ..InitOptions::default()
        };
        init(&paths, &opts).unwrap();
        assert_eq!(
            state::load_config(&paths).unwrap().coordinator.url,
            "http://127.0.0.1:18101"
        );
    }

    #[test]
    fn init_writes_a_private_config_and_token() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("state"));
        init(&paths, &InitOptions::default()).unwrap();
        let mode =
            |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(paths.config_file()), 0o600, "config.toml");
        assert_eq!(mode(paths.token_file()), 0o600, "token");
        assert_eq!(mode(paths.root().to_path_buf()), 0o700, "state dir");
    }
}
