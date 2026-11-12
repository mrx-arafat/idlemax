//! Domain: the tasks of the switched-on projects, in the order of D7 (broken checks, roadmap
//! items, assigned issues, unfinished session work), each ready or with the reason it needs
//! the owner. Pure.
//!
//! The code checks only what it can prove; whether a task is small, vague or architectural is
//! the orchestrator's judgment, and a ready task becomes a pick only when a pass picks it (A19).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::check::BaseCheck;
use super::fnv64;
use super::inputs::StoredRepo;
use super::project::{default_name, ProjectView};
use crate::advice::text::slug;
use crate::advice::{GhItem, SessionMeta, SessionSignal};
use crate::types::{DeviceId, UnixMs};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSource {
    BrokenCheck,
    Roadmap,
    GithubIssue,
    Session,
}

impl TaskSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskSource::BrokenCheck => "broken_check",
            TaskSource::Roadmap => "roadmap",
            TaskSource::GithubIssue => "github_issue",
            TaskSource::Session => "session",
        }
    }
}

/// One small piece of work in a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project: String,
    pub project_id: i64,
    pub device: DeviceId,
    pub source: TaskSource,
    pub title: String,
    pub done_check: Option<String>,
    /// Files or folders the task needs, named by the orchestrator when it picks the task
    /// (A4); empty until then.
    pub scope_paths: Vec<String>,
    /// None when the task is ready; else why it needs the owner.
    pub needs_you: Option<String>,
}

/// A task the orchestrator sent to "needs you", as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judgment {
    pub task: String,
    pub digest: String,
    pub reason: String,
}

pub struct TaskInput<'a> {
    pub now_ms: UnixMs,
    /// With `owner_active` filled.
    pub projects: &'a [ProjectView],
    pub repos: &'a [StoredRepo],
    pub checks: &'a [BaseCheck],
    pub issues: &'a [GhItem],
    pub sessions: &'a [(DeviceId, SessionMeta)],
    pub judgments: &'a [Judgment],
    /// (task id, scope paths) from the newest pass's picks.
    pub scoped: &'a [(String, Vec<String>)],
}

/// The tasks of switched-on projects, per project in source order.
pub fn discover(input: &TaskInput<'_>) -> Vec<Task> {
    let mut out = Vec::new();
    for view in input.projects.iter().filter(|v| v.project.enabled) {
        let repo = input
            .repos
            .iter()
            .find(|r| r.device == view.project.device && r.repo.path == view.project.path);
        let mut tasks = broken_check(view, repo, input.checks);
        tasks.extend(roadmap(view, repo));
        tasks.extend(issues(view, repo, input.issues));
        tasks.extend(sessions(view, input.sessions));
        for f in tasks {
            let mut task = Task {
                needs_you: readiness(view, &f, input.judgments),
                ..f.task
            };
            if let Some((_, paths)) = input.scoped.iter().find(|(id, _)| *id == task.id) {
                task.scope_paths = paths.clone();
            }
            out.push(task);
        }
    }
    out
}

/// `fnv64` of the task's source, title and done check: it changes when the task changes.
pub fn digest(task: &Task) -> String {
    fnv64(&[
        task.source.as_str(),
        &task.title,
        task.done_check.as_deref().unwrap_or(""),
    ])
}

/// A task with what the readiness rule needs to know beyond its fields.
struct Found {
    task: Task,
    owner_item: bool,
    interrupted_only: bool,
}

fn found(view: &ProjectView, source: TaskSource, id: String, title: String) -> Found {
    let p = &view.project;
    Found {
        task: Task {
            id,
            project: p.name.clone(),
            project_id: p.id,
            device: p.device.clone(),
            source,
            title,
            done_check: None,
            scope_paths: Vec::new(),
            needs_you: None,
        },
        owner_item: false,
        interrupted_only: false,
    }
}

fn check_words(view: &ProjectView) -> Option<String> {
    let argv = &view.project.settings.check_command;
    (!argv.is_empty()).then(|| argv.join(" "))
}

fn broken_check(view: &ProjectView, repo: Option<&StoredRepo>, checks: &[BaseCheck]) -> Vec<Found> {
    let commit = repo.and_then(|r| r.repo.base_commit.as_deref());
    let failed = checks.iter().any(|c| {
        c.project == view.project.id
            && Some(c.base_commit.as_str()) == commit
            && c.passed == Some(false)
    });
    let Some(words) = check_words(view).filter(|_| failed) else {
        return Vec::new();
    };
    let id = format!("broken_check:{}", view.project.id);
    let mut f = found(
        view,
        TaskSource::BrokenCheck,
        id,
        format!("Fix the failing check: {words}"),
    );
    f.task.done_check = Some(words);
    vec![f]
}

fn roadmap(view: &ProjectView, repo: Option<&StoredRepo>) -> Vec<Found> {
    let items = repo.map(|r| r.repo.roadmap.as_slice()).unwrap_or_default();
    let mut used: BTreeMap<String, u32> = BTreeMap::new();
    let mut out = Vec::new();
    for item in items.iter().filter(|i| !i.done) {
        let base = slug(&item.title);
        let n = used.entry(base.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { base } else { format!("{base}-{n}") };
        let id = format!("roadmap:{}:{key}", view.project.id);
        let mut f = found(view, TaskSource::Roadmap, id, item.title.clone());
        f.task.done_check = Some(item.done_when.clone()).filter(|d| !d.is_empty());
        f.owner_item = item.owner;
        out.push(f);
    }
    out
}

fn issues(view: &ProjectView, repo: Option<&StoredRepo>, all: &[GhItem]) -> Vec<Found> {
    let Some(slug) = repo.and_then(|r| r.repo.slug.as_deref()) else {
        return Vec::new();
    };
    let mut mine: Vec<&GhItem> = all.iter().filter(|i| i.repo == slug).collect();
    mine.sort_by_key(|i| (i.created_ms, i.number));
    mine.into_iter()
        .map(|i| {
            let id = format!("github_issue:{}", i.reference());
            let mut f = found(view, TaskSource::GithubIssue, id, i.title.clone());
            f.task.done_check = check_words(view).map(|w| format!("the focused check passes: {w}"));
            f
        })
        .collect()
}

fn sessions(view: &ProjectView, all: &[(DeviceId, SessionMeta)]) -> Vec<Found> {
    let p = &view.project;
    let folder = default_name(&p.path);
    let mut mine: Vec<&SessionMeta> = all
        .iter()
        .filter(|(d, s)| *d == p.device && s.project.as_deref() == Some(folder.as_str()))
        .map(|(_, s)| s)
        .filter(|s| {
            s.signals.contains(&SessionSignal::TestsFailing)
                || s.signals.contains(&SessionSignal::Interrupted)
        })
        .collect();
    mine.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
    mine.into_iter()
        .map(|s| {
            let failing = s.signals.contains(&SessionSignal::TestsFailing);
            let what = if failing {
                "failing tests"
            } else {
                "an interrupted turn"
            };
            let name = s.title.clone().unwrap_or_else(|| s.id.clone());
            let id = format!("session:{}:{}:{}", p.id, s.tool, s.id);
            let mut f = found(
                view,
                TaskSource::Session,
                id,
                format!("Finish {what} of session: {name}"),
            );
            if failing {
                f.task.done_check =
                    check_words(view).map(|w| format!("the focused check passes: {w}"));
            }
            f.interrupted_only = !failing;
            f
        })
        .collect()
}

/// D7's readiness rule, first match wins; each reason is the text shown to the owner.
fn readiness(view: &ProjectView, f: &Found, judgments: &[Judgment]) -> Option<String> {
    let name = &view.project.name;
    if view.project.settings.base_branch.is_none() {
        return Some(format!(
            "set the base branch: idlemax project set {name} base <branch>"
        ));
    }
    if f.task.done_check.is_none() {
        return Some(if f.interrupted_only {
            "an interrupted turn has no check that proves it done".into()
        } else {
            format!("no check proves it done: idlemax project set {name} check -- <command>")
        });
    }
    if f.owner_item {
        return Some("the item says it needs the owner".into());
    }
    if f.task.source == TaskSource::Session && view.owner_active {
        return Some("the owner is working on this now".into());
    }
    let digest = digest(&f.task);
    judgments
        .iter()
        .find(|j| j.task == f.task.id && j.digest == digest)
        .map(|j| j.reason.clone())
}
