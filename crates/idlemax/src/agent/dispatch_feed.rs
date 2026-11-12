//! The agent's dispatch feed (adapter): finds candidate repositories from Herdr and from the
//! projects the coordinator said are on, reports whether an owner session works in each, and
//! reads the roadmap and the owner's changed files of the switched-on ones. Unlike the
//! advisor feed, project paths leave the device.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use idlemax_core::dispatch::inputs::{
    DispatchInputs, DispatchReply, RepoReport, MAX_REPOS, MAX_ROADMAP_ITEMS, MAX_TOUCHED,
};
use idlemax_core::dispatch::project::{default_name, Project};
use idlemax_core::dispatch::roadmap::parse_roadmap;
use idlemax_core::types::DeviceId;
use idlemax_core::Paths;

use super::client::CoordClient;
use crate::dispatch::herdr::{self, HerdrView};
use crate::dispatch::repo;

/// What one feed run needs from the agent.
#[derive(Debug, Clone)]
pub struct FeedContext {
    pub device: DeviceId,
    pub paths: Paths,
    /// The switched-on projects of the last reply.
    pub projects: Vec<Project>,
}

/// One feed run: collect, send, and return the reply. The next tick is the retry.
pub async fn run(client: CoordClient, ctx: FeedContext) -> Option<DispatchReply> {
    let inputs = collect(&ctx, herdr::read().await).await;
    match client.dispatch_inputs(&inputs).await {
        Ok(reply) => {
            tracing::debug!(repos = inputs.repos.len(), "dispatch inputs sent");
            Some(reply)
        }
        Err(e) => {
            tracing::warn!("dispatch inputs not delivered: {e}");
            None
        }
    }
}

/// The union of the folders Herdr shows and the replied projects, each read once.
pub async fn collect(ctx: &FeedContext, herdr: Option<HerdrView>) -> DispatchInputs {
    let own = ctx.paths.root();
    let view = herdr.clone().unwrap_or_default();
    let mine = |f: &String| !inside(Path::new(f), own);
    let mut roots: Vec<String> = ctx.projects.iter().map(|p| p.path.clone()).collect();
    for folder in view.panes.iter().chain(view.agents.iter().map(|a| &a.cwd)) {
        if !mine(folder) {
            continue;
        }
        if let Some(root) = repo::root(Path::new(folder)).await {
            roots.push(root.to_string_lossy().into_owned());
        }
    }
    let mut working = BTreeSet::new();
    for agent in view.agents.iter().filter(|a| a.working && mine(&a.cwd)) {
        if let Some(root) = repo::root(Path::new(&agent.cwd)).await {
            working.insert(root.to_string_lossy().into_owned());
        }
    }
    let mut seen = BTreeSet::new();
    roots.retain(|r| plain(r) && seen.insert(r.clone()));
    roots.truncate(MAX_REPOS);
    let mut repos = Vec::new();
    for root in roots {
        let project = ctx.projects.iter().find(|p| p.path == root);
        let mut report = read_repo(&root, working.contains(&root)).await;
        if let Some(p) = project {
            fill_project(&mut report, p).await;
        }
        repos.push(report);
    }
    DispatchInputs {
        device: ctx.device.clone(),
        herdr: herdr.is_some(),
        repos,
    }
}

/// What every candidate repository reports.
async fn read_repo(root: &str, working: bool) -> RepoReport {
    let dir = PathBuf::from(root);
    RepoReport {
        path: root.to_string(),
        name: default_name(root),
        slug: repo::slug(&dir).await.filter(|s| plain(s)),
        default_branch: repo::default_branch(&dir).await.filter(|b| plain(b)),
        working,
        ..RepoReport::default()
    }
}

/// The base commit, roadmap and the owner's changed files of a switched-on project.
async fn fill_project(report: &mut RepoReport, p: &Project) {
    let dir = PathBuf::from(&p.path);
    let base = p
        .settings
        .base_branch
        .clone()
        .or_else(|| report.default_branch.clone());
    if let Some(base) = base.filter(|b| plain(b)) {
        report.base_commit = repo::base_commit(&dir, &base).await;
        if let Some(text) = repo::roadmap_text(&dir, &base).await {
            let mut items = parse_roadmap(&text);
            items.truncate(MAX_ROADMAP_ITEMS);
            report.roadmap = items;
        }
    }
    let mut touched: Vec<String> = repo::touched(&dir)
        .await
        .into_iter()
        .filter(|t| plain(t))
        .collect();
    report.touched_clipped = touched.len() > MAX_TOUCHED;
    touched.truncate(MAX_TOUCHED);
    report.touched = touched;
}

/// A folder inside `$IDLEMAX_HOME` is never the owner's.
fn inside(folder: &Path, own: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(folder).starts_with(canon(own))
}

/// No control character: the coordinator refuses one.
fn plain(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use idlemax_core::dispatch::project::ProjectSettings;

    use super::*;
    use crate::dispatch::herdr::HerdrAgent;
    use crate::dispatch::repo::tests::{git_cmd, repo};

    fn project(path: &Path) -> Project {
        Project {
            id: 1,
            name: default_name(&path.to_string_lossy()),
            device: "mac".into(),
            path: path.to_string_lossy().into_owned(),
            enabled: true,
            settings: ProjectSettings {
                base_branch: Some("main".into()),
                ..Default::default()
            },
        }
    }

    fn ctx(state: &Path, projects: Vec<Project>) -> FeedContext {
        FeedContext {
            device: "mac".into(),
            paths: Paths::new(state),
            projects,
        }
    }

    #[tokio::test]
    async fn dispatch_feed_reports_herdr_repos_and_reads_only_replied_projects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (busy, on, state) = (root.join("busy"), root.join("on"), root.join("state"));
        repo(&busy);
        repo(&on);
        std::fs::write(on.join("ROADMAP.md"), "## Do it\n\nDone when: it is done\n").unwrap();
        git_cmd(&on, &["add", "ROADMAP.md"]);
        git_cmd(&on, &["commit", "-q", "-m", "roadmap"]);
        std::fs::write(on.join("dirty.rs"), "x").unwrap();
        std::fs::write(busy.join("dirty.rs"), "x").unwrap();
        repo(&state.join("checks/1"));
        let herdr = HerdrView {
            panes: vec![
                on.to_string_lossy().into(),
                state.join("checks/1").to_string_lossy().into(),
            ],
            agents: vec![HerdrAgent {
                cwd: busy.to_string_lossy().into(),
                working: true,
            }],
        };
        let inputs = collect(&ctx(&state, vec![project(&on)]), Some(herdr)).await;
        assert!(inputs.herdr);
        let paths: Vec<&str> = inputs.repos.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, [on.to_str().unwrap(), busy.to_str().unwrap()]);
        let (on_r, busy_r) = (&inputs.repos[0], &inputs.repos[1]);
        assert!(busy_r.working && !on_r.working);
        assert_eq!(on_r.roadmap.len(), 1);
        assert_eq!(on_r.touched, ["dirty.rs"]);
        assert!(on_r.base_commit.is_some());
        assert!(busy_r.roadmap.is_empty() && busy_r.touched.is_empty());
    }

    #[tokio::test]
    async fn dispatch_feed_reads_a_replied_project_without_herdr_and_clips_touched() {
        let dir = tempfile::tempdir().unwrap();
        let on = dir.path().canonicalize().unwrap().join("on");
        repo(&on);
        for i in 0..250 {
            std::fs::write(on.join(format!("f{i}")), "x").unwrap();
        }
        let inputs = collect(&ctx(&dir.path().join("state"), vec![project(&on)]), None).await;
        assert!(!inputs.herdr);
        let r = &inputs.repos[0];
        assert!(r.base_commit.is_some());
        assert_eq!((r.touched.len(), r.touched_clipped), (200, true));
    }
}
