//! Adapter: loads job spec templates (`*.toml`, same format as `idlemax job add`) from
//! `Paths::jobs_dir()`. Invalid files are logged and skipped.

use std::path::{Path, PathBuf};

use idlemax_core::types::{JobSpec, SpecError};

#[derive(Debug, thiserror::Error)]
enum TemplateError {
    #[error("reading: {0}")]
    Read(#[from] std::io::Error),
    #[error("parsing: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid: {0}")]
    Invalid(#[from] SpecError),
}

/// Valid templates sorted by file name; a missing dir means none.
pub fn load(dir: &Path) -> Vec<JobSpec> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "job templates unreadable: {e}");
            return Vec::new();
        }
    };
    files.retain(|p| p.extension().is_some_and(|x| x == "toml"));
    files.sort();
    files
        .iter()
        .filter_map(|p| match load_one(p) {
            Ok(spec) => Some(spec),
            Err(e) => {
                tracing::warn!(file = %p.display(), "skipping job template: {e}");
                None
            }
        })
        .collect()
}

fn load_one(path: &Path) -> Result<JobSpec, TemplateError> {
    let spec: JobSpec = toml::from_str(&std::fs::read_to_string(path)?)?;
    spec.validate()?;
    Ok(spec)
}
