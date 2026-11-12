use super::*;
use crate::dispatch::check::Phase;
use crate::dispatch::inputs::RepoReport;
use crate::dispatch::project::{Project, ProjectSettings};
use crate::dispatch::roadmap::RoadmapItem;

const NOW: UnixMs = 1_800_000_000_000;

fn view(enabled: bool) -> ProjectView {
    ProjectView {
        project: Project {
            id: 7,
            name: "app".into(),
            device: "mac".into(),
            path: "/code/app".into(),
            enabled,
            settings: ProjectSettings {
                base_branch: Some("main".into()),
                check_command: vec!["cargo".into(), "test".into()],
                ..Default::default()
            },
        },
        ..Default::default()
    }
}

fn item(title: &str) -> RoadmapItem {
    RoadmapItem {
        title: title.into(),
        done_when: "it works".into(),
        ..Default::default()
    }
}

fn repo(roadmap: Vec<RoadmapItem>) -> StoredRepo {
    StoredRepo {
        device: "mac".into(),
        received_ms: NOW,
        repo: RepoReport {
            path: "/code/app".into(),
            slug: Some("me/app".into()),
            base_commit: Some("c1".into()),
            roadmap,
            ..Default::default()
        },
    }
}

fn failed_check() -> BaseCheck {
    BaseCheck {
        project: 7,
        base_commit: "c1".into(),
        phase: Phase::Check,
        job: Some(1),
        passed: Some(false),
        detail: None,
        finished_ms: Some(NOW),
    }
}

fn issue() -> GhItem {
    GhItem {
        repo: "me/app".into(),
        number: 12,
        title: "Crash on empty input".into(),
        ..Default::default()
    }
}

fn session(signal: SessionSignal) -> (DeviceId, SessionMeta) {
    let meta = SessionMeta {
        tool: "claude".into(),
        id: "s1".into(),
        title: Some("Fix login".into()),
        project: Some("app".into()),
        last_activity_ms: NOW,
        signals: vec![signal],
    };
    ("mac".to_string(), meta)
}

struct World {
    projects: Vec<ProjectView>,
    repos: Vec<StoredRepo>,
    checks: Vec<BaseCheck>,
    issues: Vec<GhItem>,
    sessions: Vec<(DeviceId, SessionMeta)>,
    judgments: Vec<Judgment>,
}

impl World {
    fn full() -> Self {
        Self {
            projects: vec![view(true)],
            repos: vec![repo(vec![item("Faster sync"), item("Dark mode")])],
            checks: vec![failed_check()],
            issues: vec![issue()],
            sessions: vec![session(SessionSignal::TestsFailing)],
            judgments: Vec::new(),
        }
    }

    fn discover(&self) -> Vec<Task> {
        discover(&TaskInput {
            now_ms: NOW,
            projects: &self.projects,
            repos: &self.repos,
            checks: &self.checks,
            issues: &self.issues,
            sessions: &self.sessions,
            judgments: &self.judgments,
            scoped: &[],
        })
    }
}

#[test]
fn the_four_sources_come_in_order_with_stable_ids() {
    let tasks = World::full().discover();
    let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "broken_check:7",
            "roadmap:7:faster-sync",
            "roadmap:7:dark-mode",
            "github_issue:me/app#12",
            "session:7:claude:s1",
        ]
    );
    assert!(tasks.iter().all(|t| t.needs_you.is_none()), "{tasks:?}");
    assert_eq!(tasks[0].title, "Fix the failing check: cargo test");
}

#[test]
fn roadmap_items_sharing_a_slug_get_a_suffix() {
    let long = "a".repeat(70);
    let mut w = World::full();
    w.repos = vec![repo(vec![
        item(&format!("{long} one")),
        item(&format!("{long} two")),
    ])];
    let tasks = w.discover();
    let slug = slug(&format!("{long} one"));
    let roadmap: Vec<&str> = tasks
        .iter()
        .filter(|t| t.source == TaskSource::Roadmap)
        .map(|t| t.id.as_str())
        .collect();
    assert_eq!(
        roadmap,
        [format!("roadmap:7:{slug}"), format!("roadmap:7:{slug}-2")]
    );
}

#[test]
fn an_off_project_has_no_task_and_no_base_branch_holds_every_task() {
    let mut w = World::full();
    w.projects = vec![view(false)];
    assert!(w.discover().is_empty());
    w.projects[0].project.enabled = true;
    w.projects[0].project.settings.base_branch = None;
    let tasks = w.discover();
    assert!(!tasks.is_empty());
    for t in tasks {
        assert_eq!(
            t.needs_you.as_deref(),
            Some("set the base branch: idlemax project set app base <branch>")
        );
    }
}

#[test]
fn tasks_without_a_check_or_with_the_owner_need_the_owner() {
    let mut w = World::full();
    w.projects[0].project.settings.check_command.clear();
    w.checks.clear();
    w.repos = vec![repo(vec![RoadmapItem {
        owner: true,
        ..item("Ship it")
    }])];
    w.sessions = vec![session(SessionSignal::Interrupted)];
    let reasons: Vec<(TaskSource, String)> = w
        .discover()
        .into_iter()
        .map(|t| (t.source, t.needs_you.unwrap_or_default()))
        .collect();
    assert_eq!(
        reasons,
        [
            (
                TaskSource::Roadmap,
                "the item says it needs the owner".into()
            ),
            (
                TaskSource::GithubIssue,
                "no check proves it done: idlemax project set app check -- <command>".into()
            ),
            (
                TaskSource::Session,
                "an interrupted turn has no check that proves it done".into()
            ),
        ]
    );

    let mut w = World::full();
    w.projects[0].owner_active = true;
    let session = w.discover().pop().unwrap();
    assert_eq!(
        session.needs_you.as_deref(),
        Some("the owner is working on this now")
    );
}

#[test]
fn a_judgment_holds_until_its_task_changes() {
    let mut w = World::full();
    let issue = w.discover().remove(3);
    w.judgments = vec![Judgment {
        task: issue.id.clone(),
        digest: digest(&issue),
        reason: "too vague".into(),
    }];
    assert_eq!(w.discover()[3].needs_you.as_deref(), Some("too vague"));
    w.issues[0].title = "Crash on empty input, with a test".into();
    assert_eq!(w.discover()[3].needs_you, None);
}
