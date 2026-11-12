//! Delivery: `idlemax inbox` subcommands: list job and notice notes and show one.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use idlemax_core::job_run::inbox::{parse_note, sort_entries, InboxEntry};
use idlemax_core::types::JobId;
use idlemax_core::Paths;

use super::out::{hints, lists};
use super::{print, CliError, Out};

/// Reads all notes from `$IDLEMAX_HOME/inbox`, returning them sorted newest first.
pub fn load_notes(inbox_dir: &Path) -> Result<Vec<InboxEntry>> {
    if !inbox_dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(inbox_dir)
        .with_context(|| format!("reading inbox dir {}", inbox_dir.display()))?;

    for entry in read_dir {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let fname = entry.file_name();
        let fname_str = fname.to_string_lossy();
        if fname_str.starts_with('.') || !fname_str.ends_with(".md") {
            continue;
        }
        let path = entry.path();
        if let Ok(content) = fs::read_to_string(&path) {
            if let Some(note) = parse_note(&fname_str, &content) {
                entries.push(note);
            }
        }
    }

    sort_entries(&mut entries);
    Ok(entries)
}

/// Runs `idlemax inbox [list]`.
pub fn run_list(paths: &Paths, out: &Out) -> Result<()> {
    let entries = load_notes(&paths.inbox_dir())?;
    out.emit_with(
        &entries,
        |entries| table_text(entries),
        || Ok(lists::inbox(&entries, out.fields())?),
        &hints::inbox(entries.len()),
    )
}

fn table_text(entries: &[InboxEntry]) -> String {
    if entries.is_empty() {
        return "(inbox is empty)".into();
    }
    let header = ["TIME", "JOB", "NAME", "RESULT"];
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                e.time.clone(),
                e.reference(),
                e.name.clone(),
                e.result.clone(),
            ]
        })
        .collect();
    print::table(&header, &rows)
}

/// A note as `idlemax inbox show` takes it: a job id, or `n<id>` for a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRef {
    Job(JobId),
    Notice(i64),
}

pub fn parse_note_ref(text: &str) -> std::result::Result<NoteRef, String> {
    let bad = || format!("`{text}` is not a note: give a job id such as 42 or a notice such as n7");
    match text.strip_prefix('n') {
        Some(n) => n.parse().map(NoteRef::Notice).map_err(|_| bad()),
        None => text.parse().map(NoteRef::Job).map_err(|_| bad()),
    }
}

/// Runs `idlemax inbox show <job-id | n<notice-id>>`.
pub fn run_show(paths: &Paths, id: NoteRef, out: &Out) -> Result<()> {
    let entries = load_notes(&paths.inbox_dir())?;
    let matching = entries.into_iter().find(|e| match id {
        NoteRef::Job(j) => e.job_id == Some(j),
        NoteRef::Notice(n) => e.notice == Some(n),
    });
    let Some(entry) = matching else {
        let what = match id {
            NoteRef::Job(j) => format!("job {j}"),
            NoteRef::Notice(n) => format!("notice n{n}"),
        };
        return Err(CliError::runtime(
            format!("no inbox note found for {what}"),
            "idlemax inbox (lists the notes there are)",
        )
        .into());
    };
    let note_path = paths.inbox_dir().join(&entry.file_name);
    let content = fs::read_to_string(&note_path)
        .with_context(|| format!("reading note at {}", note_path.display()))?;
    out.raw(&content)
}
