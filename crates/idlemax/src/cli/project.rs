//! Delivery: `idlemax project list|on|off|add|set`, the projects the dispatcher may work in.
//! Every project is off until the owner switches it on (D5); nothing is written into the
//! owner's repositories.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use idlemax_core::dispatch::project::{NewProject, ProjectSettings, ProjectView};

use super::out::dispatch as views;
use super::{noop, ApiClient, CliError, Out};

#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    /// List the projects idlemax knows; every one is off until you switch it on
    List {
        #[arg(long)]
        device: Option<String>,
    },
    /// Let the dispatcher plan work in a project
    On {
        name: String,
        #[arg(long)]
        device: Option<String>,
    },
    /// Take a project out of the dispatcher's pool
    Off {
        name: String,
        #[arg(long)]
        device: Option<String>,
    },
    /// Add the git checkout at PATH as a project (it starts off)
    Add {
        path: PathBuf,
        /// [default: the repository's folder name]
        #[arg(long)]
        name: Option<String>,
        /// [default: the only device the coordinator knows]
        #[arg(long)]
        device: Option<String>,
    },
    /// Change one setting: base, priority, off-limits, core-paths, or setup and check (after `--`)
    Set {
        name: String,
        setting: SettingName,
        /// The value; lists take several words or commas; none clears the setting
        values: Vec<String>,
        /// The setup or check command, after `--`; run directly, never through a shell
        #[arg(last = true, value_name = "ARGV")]
        argv: Vec<String>,
        #[arg(long)]
        device: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SettingName {
    Base,
    Priority,
    OffLimits,
    CorePaths,
    Setup,
    Check,
}

pub async fn run(cmd: ProjectCmd, c: &ApiClient, out: &Out) -> Result<()> {
    match cmd {
        ProjectCmd::List { device } => {
            let mut all = c.projects().await?;
            if let Some(d) = &device {
                all.retain(|v| &v.project.device == d);
            }
            let with_device = device.is_none();
            out.emit_with(
                &all,
                |v| views::project_table(v, with_device),
                || Ok(views::project_list(&all)),
                &views::projects_hints(&all),
            )
        }
        ProjectCmd::On { name, device } => switch(c, &name, device, true, out).await,
        ProjectCmd::Off { name, device } => switch(c, &name, device, false, out).await,
        ProjectCmd::Add { path, name, device } => {
            let root = repo_root(&path).await?;
            let new = NewProject {
                path: root,
                device,
                name,
            };
            let (view, created) = c.add_project(&new).await?;
            let result = if created {
                "added (off)"
            } else {
                "already known"
            };
            emit(out, result, &view)
        }
        ProjectCmd::Set {
            name,
            setting,
            values,
            argv,
            device,
        } => {
            let view = find(c, &name, device.as_deref()).await?;
            let mut settings = view.project.settings.clone();
            let shown = apply(&mut settings, setting, values, argv)?;
            let view = c.set_project_settings(view.project.id, &settings).await?;
            emit(out, &shown, &view)
        }
    }
}

async fn switch(
    c: &ApiClient,
    name: &str,
    device: Option<String>,
    on: bool,
    out: &Out,
) -> Result<()> {
    let view = find(c, name, device.as_deref()).await?;
    let state = if on { "on" } else { "off" };
    if view.project.enabled == on {
        return emit(out, &noop::already(state), &view);
    }
    let view = c.switch_project(view.project.id, on).await?;
    emit(out, state, &view)
}

fn emit(out: &Out, result: &str, view: &ProjectView) -> Result<()> {
    out.emit_with(
        view,
        |v| views::project_line(result, v),
        || Ok(views::project_result(result, view)),
        &views::project_hints(view),
    )
}

/// The project of that name, on the given device or the only device holding it.
async fn find(c: &ApiClient, name: &str, device: Option<&str>) -> Result<ProjectView> {
    let mut found: Vec<ProjectView> = c
        .projects()
        .await?
        .into_iter()
        .filter(|v| v.project.name == name && device.is_none_or(|d| v.project.device == d))
        .collect();
    match found.len() {
        0 => {
            Err(CliError::usage(format!("no project named {name}"), "idlemax project list").into())
        }
        1 => Ok(found.remove(0)),
        _ => {
            let devices: Vec<String> = found.into_iter().map(|v| v.project.device).collect();
            Err(CliError::usage(
                format!(
                    "{name} exists on more than one device: {}",
                    devices.join(", ")
                ),
                format!("pass --device {}", devices[0]),
            )
            .into())
        }
    }
}

/// Changes one setting and returns how to show it, such as `base = main`.
fn apply(
    s: &mut ProjectSettings,
    setting: SettingName,
    values: Vec<String>,
    argv: Vec<String>,
) -> Result<String> {
    let usage = |what: &str| -> anyhow::Error {
        CliError::usage(what.to_string(), "idlemax project set --help").into()
    };
    let command_setting = matches!(setting, SettingName::Setup | SettingName::Check);
    if command_setting && !values.is_empty() {
        return Err(usage(
            "give the command after `--`, as in `check -- cargo test`",
        ));
    }
    if !command_setting && !argv.is_empty() {
        return Err(usage("only setup and check take a command after `--`"));
    }
    let list = |values: Vec<String>| -> Vec<String> {
        values
            .iter()
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect()
    };
    let shown = match setting {
        SettingName::Base => {
            if values.len() > 1 {
                return Err(usage("base takes one branch name"));
            }
            s.base_branch = values.into_iter().next();
            format!("base = {}", s.base_branch.as_deref().unwrap_or("-"))
        }
        SettingName::Priority => {
            let [value] = values.as_slice() else {
                return Err(usage("priority takes one whole number"));
            };
            s.priority = value
                .parse()
                .map_err(|_| usage("priority takes one whole number"))?;
            format!("priority = {}", s.priority)
        }
        SettingName::OffLimits => {
            s.off_limits = list(values);
            format!("off-limits = {}", views::words(&s.off_limits))
        }
        SettingName::CorePaths => {
            s.core_paths = list(values);
            format!("core-paths = {}", views::words(&s.core_paths))
        }
        SettingName::Setup => {
            s.setup_command = argv;
            format!("setup = {}", views::words(&s.setup_command))
        }
        SettingName::Check => {
            s.check_command = argv;
            format!("check = {}", views::words(&s.check_command))
        }
    };
    Ok(shown)
}

/// The main working tree of the repository that holds `path`, on this machine.
async fn repo_root(path: &Path) -> Result<String> {
    let not_git = || -> anyhow::Error {
        CliError::usage(
            format!("{} is not inside a git repository", path.display()),
            "pass the folder of a git checkout",
        )
        .into()
    };
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|_| not_git())?;
    if !output.status.success() {
        return Err(not_git());
    }
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let common = Path::new(&common);
    match (common.file_name(), common.parent()) {
        (Some(n), Some(root)) if n == ".git" => Ok(root.to_string_lossy().into_owned()),
        _ => Err(not_git()),
    }
}
