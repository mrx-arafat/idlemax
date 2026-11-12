//! Domain: a project is a git repository on a device that idlemax knows about, off until the
//! owner switches it on (D5), with the owner's per-project settings (D18.3, A4). Pure.

use serde::{Deserialize, Serialize};

use crate::advice::SessionMeta;
use crate::protocol::name_problem;
use crate::types::{DeviceId, UnixMs};

/// Longest project path and name, in characters.
pub const MAX_PATH_CHARS: usize = 4096;
pub const MAX_NAME_CHARS: usize = 128;
/// Most projects one device may hold.
pub const MAX_PROJECTS_PER_DEVICE: i64 = 200;
/// Most entries in `off_limits` or in `core_paths`.
pub const MAX_PATH_LIST: usize = 64;
const MAX_BRANCH_CHARS: usize = 200;

/// The owner's settings of one project. Commands are argv, never a shell line (A4).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    pub base_branch: Option<String>,
    pub setup_command: Vec<String>,
    pub check_command: Vec<String>,
    /// Paths, relative to the repository root, a worker must not touch.
    pub off_limits: Vec<String>,
    /// The owner's list of shared or core paths, for the risk rule of D8 (A4).
    pub core_paths: Vec<String>,
    /// Higher is picked first.
    pub priority: i32,
    /// The owner switched `DEFAULT_OFF_LIMITS` off for this project.
    pub no_default_off_limits: bool,
}

/// Paths no worker touches unless the owner says otherwise: CI and release config, secret
/// files, and agent or tool config that runs code for the owner later (hooks, MCP servers).
pub const DEFAULT_OFF_LIMITS: [&str; 14] = [
    ".github/",
    ".gitlab-ci.yml",
    "*.env",
    "secrets/",
    ".claude/",
    ".mcp.json",
    ".husky/",
    ".githooks/",
    ".git/hooks/",
    ".envrc",
    ".circleci/",
    ".env.*",
    "*.pem",
    "*.key",
];

impl ProjectSettings {
    /// The owner's off-limits paths, then the defaults the owner did not switch off or list.
    pub fn all_off_limits(&self) -> Vec<String> {
        let mut all = self.off_limits.clone();
        let listed = |d: &str| {
            let d = d.trim_end_matches('/');
            all.iter().any(|o| o.trim_end_matches('/') == d)
        };
        if !self.no_default_off_limits {
            let missing: Vec<String> = DEFAULT_OFF_LIMITS
                .iter()
                .filter(|d| !listed(d))
                .map(|d| d.to_string())
                .collect();
            all.extend(missing);
        }
        all
    }

    /// The first reason these settings must be refused, if any.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(branch) = &self.base_branch {
            branch_problem(branch)?;
        }
        for (field, argv) in [
            ("setup command", &self.setup_command),
            ("check command", &self.check_command),
        ] {
            if argv.first().is_some_and(|w| w.trim().is_empty()) {
                return Err(format!("the {field} has an empty first word"));
            }
            if argv.iter().any(|w| w.contains('\0')) {
                return Err(format!("the {field} contains a NUL byte"));
            }
        }
        for (field, list) in [
            ("off-limits", &self.off_limits),
            ("core-paths", &self.core_paths),
        ] {
            if list.len() > MAX_PATH_LIST {
                return Err(format!("{field} has more than {MAX_PATH_LIST} entries"));
            }
            for entry in list {
                relative_path_problem(entry).map_err(|e| format!("{field}: {e}"))?;
            }
        }
        Ok(())
    }
}

/// A branch name is passed to git as one argument, so it must not look like an option.
fn branch_problem(branch: &str) -> Result<(), String> {
    if branch.is_empty() {
        return Err("the base branch is empty".into());
    }
    if branch.chars().count() > MAX_BRANCH_CHARS {
        return Err(format!(
            "the base branch is longer than {MAX_BRANCH_CHARS} characters"
        ));
    }
    if branch.starts_with('-') {
        return Err(format!("the base branch {branch:?} starts with '-'"));
    }
    if branch.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "the base branch {branch:?} holds whitespace or a control character"
        ));
    }
    Ok(())
}

/// A path inside the repository: relative, with no `..` component.
pub fn relative_path_problem(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("a path is empty".into());
    }
    if let Some(p) = name_problem("a path", path, MAX_PATH_CHARS) {
        return Err(p);
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(format!(
            "{path:?} is absolute; give it relative to the repository"
        ));
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("{path:?} holds a '..' component"));
    }
    Ok(())
}

/// One project.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub device: DeviceId,
    /// The repository's main working tree, absolute.
    pub path: String,
    pub enabled: bool,
    #[serde(flatten)]
    pub settings: ProjectSettings,
}

/// How idlemax learned of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSource {
    /// A folder the owner's Herdr session showed.
    Herdr,
    /// Added by the owner with `idlemax project add`.
    #[default]
    Owner,
}

impl ProjectSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectSource::Herdr => "herdr",
            ProjectSource::Owner => "owner",
        }
    }
}

/// A project as the API shows it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectView {
    #[serde(flatten)]
    pub project: Project,
    pub source: ProjectSource,
    /// The owner is working in this project now or was recently (D5).
    pub owner_active: bool,
    /// When an owner agent session was last seen working here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_active_ms: Option<UnixMs>,
    /// When a device last reported this repository.
    pub seen_ms: Option<UnixMs>,
    /// Something the owner should fix, such as a program missing from the allowlist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// `POST /v1/projects`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewProject {
    pub path: String,
    /// The only known device when absent.
    pub device: Option<DeviceId>,
    /// The last path component when absent.
    pub name: Option<String>,
}

impl NewProject {
    pub fn check(&self) -> Result<(), String> {
        if !self.path.starts_with('/') {
            return Err(format!("the path {:?} is not absolute", self.path));
        }
        if let Some(p) = name_problem("the path", &self.path, MAX_PATH_CHARS) {
            return Err(p);
        }
        name_check(&self.name())
    }

    /// The name given, or the last component of the path.
    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| default_name(&self.path))
    }
}

/// The last path component, or "project" for `/`.
pub fn default_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("project")
        .to_string()
}

/// A project name is shown in tables and job names: plain, short and not empty.
pub fn name_check(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("the project name is empty".into());
    }
    match name_problem("the project name", name, MAX_NAME_CHARS) {
        Some(p) => Err(p),
        None => Ok(()),
    }
}

/// "Recently" for owner activity (A25): the quota rule's interactive hold, 30 minutes.
pub const OWNER_ACTIVE_WINDOW_MS: i64 = 30 * 60_000;

/// The owner is active in a project when an owner agent session there was working within the
/// window, or a session of the same device in a folder of the project's name was active (D5).
pub fn owner_active(
    view: &ProjectView,
    sessions: &[(DeviceId, SessionMeta)],
    now_ms: UnixMs,
) -> bool {
    let recent = |at: UnixMs| now_ms.saturating_sub(at) < OWNER_ACTIVE_WINDOW_MS;
    let p = &view.project;
    let folder = default_name(&p.path);
    view.owner_active_ms.is_some_and(recent)
        || sessions.iter().any(|(device, s)| {
            device == &p.device
                && s.project.as_deref() == Some(folder.as_str())
                && recent(s.last_activity_ms)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_release_and_secret_paths_are_off_limits_until_the_owner_turns_that_off() {
        let s = ProjectSettings {
            off_limits: vec!["secrets".into(), "migrations/".into()],
            ..Default::default()
        };
        assert_eq!(
            s.all_off_limits(),
            [
                "secrets",
                "migrations/",
                ".github/",
                ".gitlab-ci.yml",
                "*.env",
                ".claude/",
                ".mcp.json",
                ".husky/",
                ".githooks/",
                ".git/hooks/",
                ".envrc",
                ".circleci/",
                ".env.*",
                "*.pem",
                "*.key"
            ]
        );
        // Agent and tool config that runs code for the owner later, and secret files.
        let changed: Vec<String> = [
            ".claude/settings.json",
            ".mcp.json",
            ".husky/pre-commit",
            ".git/hooks/post-merge",
            "app/.envrc",
            ".env.local",
            "deploy/server.pem",
            "certs/tls.key",
            "src/main.rs",
        ]
        .iter()
        .map(|p| p.to_string())
        .collect();
        let found = crate::worker::checks::scope_findings(&changed, &[], &s.all_off_limits());
        assert_eq!(found.off_limits, changed[..8]);
        let off = ProjectSettings {
            no_default_off_limits: true,
            ..s
        };
        assert_eq!(off.all_off_limits(), ["secrets", "migrations/"]);
    }

    #[test]
    fn settings_refuse_option_like_branches_and_paths_outside_the_repository() {
        let bad = [
            ProjectSettings {
                base_branch: Some("-x".into()),
                ..Default::default()
            },
            ProjectSettings {
                off_limits: vec!["/etc".into()],
                ..Default::default()
            },
            ProjectSettings {
                off_limits: vec!["a/../b".into()],
                ..Default::default()
            },
            ProjectSettings {
                core_paths: vec!["../shared".into()],
                ..Default::default()
            },
            ProjectSettings {
                check_command: vec![" ".into()],
                ..Default::default()
            },
        ];
        for s in bad {
            assert!(s.validate().is_err(), "{s:?}");
        }
        let good = ProjectSettings {
            base_branch: Some("main".into()),
            check_command: ["cargo", "test", "-p", "core"].map(String::from).to_vec(),
            ..Default::default()
        };
        assert_eq!(good.validate(), Ok(()));
    }

    #[test]
    fn a_new_project_needs_an_absolute_path_and_is_named_after_its_folder() {
        let new = NewProject {
            path: "/home/me/app/".into(),
            ..Default::default()
        };
        assert_eq!((new.check(), new.name()), (Ok(()), "app".to_string()));
        let relative = NewProject {
            path: "app".into(),
            ..Default::default()
        };
        assert!(relative.check().is_err());
    }
}
