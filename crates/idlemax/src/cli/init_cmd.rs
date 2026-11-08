//! Delivery: `idlemax init`: its arguments and the report it prints. `init` does the work.

use std::io::Read;

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use idlemax_core::types::DeviceProfile;
use idlemax_core::Paths;
use serde::Serialize;

use super::{init, out, Out};

#[derive(Args, Debug)]
pub(super) struct InitArgs {
    /// Guard preset [default: from the OS]
    #[arg(long, value_enum)]
    profile: Option<ProfileArg>,
    /// Coordinator URL agents and the CLI use, e.g. http://100.64.0.1:7878
    #[arg(long, value_name = "URL")]
    coord_url: Option<String>,
    /// Read the shared coordinator token from stdin instead of generating one
    #[arg(long)]
    token_stdin: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub(super) enum ProfileArg {
    MacosLaptop,
    LinuxServer,
}

impl From<ProfileArg> for DeviceProfile {
    fn from(p: ProfileArg) -> Self {
        match p {
            ProfileArg::MacosLaptop => DeviceProfile::MacosLaptop,
            ProfileArg::LinuxServer => DeviceProfile::LinuxServer,
        }
    }
}

pub(super) fn run_init(paths: &Paths, args: InitArgs, out: &Out) -> Result<()> {
    let token = if args.token_stdin {
        Some(read_token_stdin()?)
    } else {
        None
    };
    let opts = init::InitOptions {
        profile: args.profile.map(Into::into),
        coord_url: args.coord_url,
        token,
    };
    let report = init::init(paths, &opts)?;
    let word = |o: init::Outcome| {
        if o == init::Outcome::Created {
            "created"
        } else {
            "kept"
        }
    };
    let files = [
        Entry::new("config", word(report.config), paths.config_file()),
        Entry::new("token", word(report.token), paths.token_file()),
    ];
    out.emit_with(
        &files,
        |files| {
            let lines: Vec<String> = files
                .iter()
                .map(|f| format!("{} {}", f.action, f.path.display()))
                .collect();
            lines.join("\n")
        },
        || {
            Ok(out::toon::Node::obj([(
                "files",
                out::toon::Node::of(&files)?,
            )]))
        },
        &out::hints::init(),
    )
}

/// What `init` did to one file.
#[derive(Serialize)]
struct Entry {
    file: &'static str,
    action: &'static str,
    path: std::path::PathBuf,
}

impl Entry {
    fn new(file: &'static str, action: &'static str, path: std::path::PathBuf) -> Self {
        Self { file, action, path }
    }
}

pub(super) fn read_token_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .context("reading token from stdin")?;
    let t = s.trim();
    if t.is_empty() {
        bail!("empty token on stdin");
    }
    Ok(t.to_string())
}
