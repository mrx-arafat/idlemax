//! Domain: a project is a git repository on a device that idlemax knows about, off until the
//! owner switches it on (D5), with the owner's per-project settings (D18.3, A4). Pure.

use serde::{Deserialize, Serialize};

use crate::protocol::name_problem;
use crate::types::{DeviceId, UnixMs};

/// Longest project path and name, in characters.
pub const MAX_PATH_CHARS: usize = 4096;
pub const MAX_NAME_CHARS: usize = 128;
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
}

impl ProjectSettings {
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

#[cfg(test)]
mod tests {
    use super::*;

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
