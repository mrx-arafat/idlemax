//! Delivery: `idlemax schedule ...`, the recurring-job commands and their text output.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use idlemax_core::schedule::{Decision, Every, NewSchedule, ScheduleId, ScheduleView};
use idlemax_core::types::{JobSpec, UnixMs};
use serde::Deserialize;

use super::print::table;
use super::rows;
use super::{hint, ApiClient, Out, COORD_DOWN};

#[derive(Subcommand, Debug)]
pub enum ScheduleCmd {
    /// Repeat the job in a TOML spec file; the guard still runs it only when the machine is idle
    Add {
        spec: PathBuf,
        /// How often: 30m, 6h, 1d (minimum 1m) [default: `every` in the spec file]
        #[arg(long)]
        every: Option<Every>,
    },
    /// List schedules and what each is waiting for
    List,
    /// Remove a schedule (a job it already queued is left alone)
    Rm {
        id: ScheduleId,
    },
    /// Queue no new jobs from a schedule until it is resumed
    Pause {
        id: ScheduleId,
    },
    Resume {
        id: ScheduleId,
    },
}

pub async fn run(cmd: ScheduleCmd, c: &ApiClient, out: &Out, now: UnixMs) -> Result<()> {
    let result: Result<()> = async {
        match cmd {
            ScheduleCmd::Add { spec, every } => {
                let new = read_new_schedule(&spec, every)?;
                let v = c.add_schedule(&new).await?;
                out.emit(&v, |v| added(v, now), &[])
            }
            ScheduleCmd::List => out.emit(&c.schedules().await?, |v| list(v, now), &[]),
            ScheduleCmd::Rm { id } => out.emit(&c.remove_schedule(id).await?, removed, &[]),
            ScheduleCmd::Pause { id } => {
                out.emit(&c.pause_schedule(id).await?, |v| line(v, "paused"), &[])
            }
            ScheduleCmd::Resume { id } => {
                out.emit(&c.resume_schedule(id).await?, |v| line(v, "resumed"), &[])
            }
        }
    }
    .await;
    result.map_err(|e| hint(e, COORD_DOWN))
}

/// The one key a spec file may carry beyond a `JobSpec`.
#[derive(Deserialize)]
struct RepeatKey {
    every: Option<Every>,
}

/// Reads a job spec file for `schedule add`. The flag wins over the file's `every`.
fn read_new_schedule(path: &Path, flag: Option<Every>) -> Result<NewSchedule> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let spec: JobSpec =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    spec.validate()
        .with_context(|| format!("invalid job spec {}", path.display()))?;
    let in_file = toml::from_str::<RepeatKey>(&text)
        .with_context(|| format!("parsing {}", path.display()))?
        .every;
    match flag.or(in_file) {
        Some(every) => Ok(NewSchedule { spec, every }),
        None => bail!(
            "no interval: pass --every (for example --every 1d) or add `every = \"1d\"` to {}",
            path.display()
        ),
    }
}

/// True when a job spec file sets `every`, which `job add` ignores.
pub fn sets_every(spec_text: &str) -> bool {
    spec_text
        .parse::<toml::Table>()
        .is_ok_and(|t| t.contains_key("every"))
}

fn added(v: &ScheduleView, now: UnixMs) -> String {
    format!(
        "schedule {} {}: repeats every {}, {}",
        v.schedule.id,
        v.schedule.spec.name,
        v.schedule.every,
        rows::schedule_state(v, now)
    )
}

fn removed(v: &ScheduleView) -> String {
    let mut text = format!(
        "removed schedule {} ({})",
        v.schedule.id, v.schedule.spec.name
    );
    if let Decision::Running { job, status } = v.decision {
        text.push_str(&format!(
            "\njob {job} is still {status}; stop it with `idlemax job cancel {job}`"
        ));
    }
    text
}

fn line(v: &ScheduleView, what: &str) -> String {
    format!(
        "schedule {} ({}) {what}",
        v.schedule.id, v.schedule.spec.name
    )
}

fn list(views: &[ScheduleView], now: UnixMs) -> String {
    if views.is_empty() {
        return "no schedules; add one with `idlemax schedule add <spec.toml> --every 1d`".into();
    }
    let rows: Vec<_> = views.iter().map(|v| rows::schedule_cells(v, now)).collect();
    table(&rows::SCHEDULE_HEADER, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_file(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("job.toml");
        std::fs::write(
            &path,
            format!("name = \"health\"\ncommand = [\"true\"]\n{body}"),
        )
        .unwrap();
        path
    }

    #[test]
    fn the_flag_beats_the_file_and_a_missing_interval_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let with_key = spec_file(dir.path(), "every = \"1d\"\n");
        let from_file = read_new_schedule(&with_key, None).unwrap();
        assert_eq!(from_file.every.to_string(), "1d");
        let flag = Some("6h".parse().unwrap());
        assert_eq!(
            read_new_schedule(&with_key, flag)
                .unwrap()
                .every
                .to_string(),
            "6h"
        );

        let without = spec_file(dir.path(), "");
        let err = read_new_schedule(&without, None).unwrap_err().to_string();
        assert!(
            err.contains("--every") && err.contains("every = \"1d\""),
            "{err}"
        );
        assert!(sets_every("name = \"x\"\nevery = \"1d\"\n"));
        assert!(!sets_every("name = \"x\"\n"));

        let bad = spec_file(dir.path(), "every = \"soon\"\n");
        let err = format!("{:#}", read_new_schedule(&bad, None).unwrap_err());
        assert!(err.contains("for example 90m, 6h or 1d"), "{err}");
    }
}
