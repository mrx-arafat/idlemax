//! The agent's dispatch feed (adapter): finds candidate repositories from Herdr and from the
//! projects the coordinator said are on, reports whether an owner session works in each, and
//! reads the roadmap and the owner's changed files of the switched-on ones. Unlike the
//! advisor feed, project paths leave the device.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use idlemax_core::dispatch::inputs::{
    Checkout, DispatchInputs, DispatchReply, RepoReport, MAX_FILES, MAX_REPOS, MAX_ROADMAP_ITEMS,
    MAX_TOUCHED,
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
    /// The worker room is `run`, so a base checkout may be made or moved.
    pub room_run: bool,
    /// A local pause is set (the kill switch file is checked at run time).
    pub local_paused: bool,
    /// Where the running jobs work; a checkout in use is never moved.
    pub job_dirs: Vec<PathBuf>,
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
            report.checkout = base_checkout(ctx, p, &report).await;
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

/// The base commit, tracked files, roadmap and the owner's changed files of a switched-on
/// project.
async fn fill_project(report: &mut RepoReport, p: &Project) {
    let dir = PathBuf::from(&p.path);
    let base = p
        .settings
        .base_branch
        .clone()
        .or_else(|| report.default_branch.clone());
    let base = base.filter(|b| plain(b));
    // Without a known base the roadmap is read at HEAD, so its items still reach the owner
    // with the reason "set the base branch"; no base commit is reported, so no check runs.
    let rev = match &base {
        Some(b) => {
            report.base_commit = repo::base_commit(&dir, b).await;
            format!("refs/heads/{b}")
        }
        None => "HEAD".to_string(),
    };
    let mut files: Vec<String> = repo::tracked_files(&dir, &rev)
        .await
        .into_iter()
        .filter(|f| plain(f))
        .collect();
    report.files_cut = files.len().saturating_sub(MAX_FILES);
    files.truncate(MAX_FILES);
    report.files = files;
    if let Some(text) = repo::roadmap_text(&dir, &rev).await {
        let mut items = parse_roadmap(&text);
        items.truncate(MAX_ROADMAP_ITEMS);
        report.roadmap = items;
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

/// The project's checkout under `checks/<id>`, made or moved to the base commit only while
/// the room is `run`, no pause or kill switch is set and no running job works in it; else
/// reported as it is. The check never runs in the owner's folder (D6, D7).
async fn base_checkout(ctx: &FeedContext, p: &Project, report: &RepoReport) -> Option<Checkout> {
    let commit = report.base_commit.as_deref()?;
    if p.settings.check_command.is_empty() {
        return None;
    }
    let dir = ctx.paths.checks_dir().join(p.id.to_string());
    let project = Path::new(&p.path);
    let in_use = ctx.job_dirs.iter().any(|d| inside(d, &dir));
    let may_write =
        ctx.room_run && !ctx.local_paused && !ctx.paths.pause_file().exists() && !in_use;
    Some(if may_write {
        repo::prepare_checkout(project, &dir, commit).await
    } else {
        repo::checkout_state(project, &dir).await
    })
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
            room_run: true,
            local_paused: false,
            job_dirs: Vec::new(),
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
        // The tracked files at the base commit, so a pass names real scope paths.
        assert!(
            on_r.files.contains(&"ROADMAP.md".to_string()),
            "{:?}",
            on_r.files
        );
        assert!(!on_r.files.contains(&"dirty.rs".to_string()));
        assert!(busy_r.files.is_empty());
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

    #[tokio::test]
    async fn dispatch_feed_keeps_the_base_checkout_at_the_base_commit_without_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (on, state) = (root.join("on"), root.join("state"));
        repo(&on);
        let marker = root.join("hook-ran");
        let hook = on.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut p = project(&on);
        p.settings.check_command = vec!["true".into()];
        std::fs::create_dir_all(&state).unwrap();
        let mut c = ctx(&state, vec![p]);
        let head = |d: &Path| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(d)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        std::fs::write(c.paths.pause_file(), "").unwrap();
        let held = collect(&c, None).await;
        assert_eq!(held.repos[0].checkout.as_ref().unwrap().commit, None);
        std::fs::remove_file(c.paths.pause_file()).unwrap();

        let made = collect(&c, None).await;
        let checkout = made.repos[0].checkout.clone().unwrap();
        assert_eq!(checkout.error, None);
        assert_eq!(checkout.commit, Some(head(&on)));
        assert_eq!(Path::new(&checkout.path), state.join("checks/1"));

        std::fs::write(on.join("next"), "x").unwrap();
        git_cmd(&on, &["add", "next"]);
        git_cmd(&on, &["commit", "-q", "-m", "next"]);
        c.local_paused = true;
        let paused = collect(&c, None).await;
        assert_ne!(
            paused.repos[0].checkout.as_ref().unwrap().commit,
            Some(head(&on))
        );
        c.local_paused = false;
        let moved = collect(&c, None).await;
        assert_eq!(
            moved.repos[0].checkout.as_ref().unwrap().commit,
            Some(head(&on))
        );
        assert!(!marker.exists(), "a repository hook ran");
    }

    #[tokio::test]
    async fn dispatch_feed_reads_the_roadmap_at_head_when_no_base_branch_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let on = dir.path().canonicalize().unwrap().join("on");
        repo(&on);
        std::fs::write(
            on.join("ROADMAP.md"),
            "## Do it\n\nDone when: `true` passes.\n",
        )
        .unwrap();
        git_cmd(&on, &["add", "ROADMAP.md"]);
        git_cmd(&on, &["commit", "-q", "-m", "roadmap"]);
        let mut p = project(&on);
        p.settings.base_branch = None;
        let inputs = collect(&ctx(&dir.path().join("state"), vec![p]), None).await;
        let r = &inputs.repos[0];
        assert_eq!(r.default_branch, None);
        assert_eq!(r.roadmap.len(), 1, "{r:?}");
        assert_eq!(r.base_commit, None);
    }
}
