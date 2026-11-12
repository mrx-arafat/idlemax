//! Domain: what each agent sends the coordinator about its git repositories (the dispatch
//! feed), the reply, and the size and character checks. Pure.
//!
//! Every type takes `#[serde(default)]` so an older or newer agent still decodes.

use serde::{Deserialize, Serialize};

use super::project::{Project, MAX_NAME_CHARS, MAX_PATH_CHARS};
use super::roadmap::{RoadmapItem, MAX_DONE_WHEN_CHARS};
use crate::protocol::{name_problem, MAX_DEVICE_ID_CHARS};
use crate::types::{DeviceId, UnixMs};

pub const MAX_REPOS: usize = 64;
pub const MAX_ROADMAP_ITEMS: usize = 50;
pub const MAX_TOUCHED: usize = 200;
/// Tracked files a project reports at most, so a pass names real scope paths.
pub const MAX_FILES: usize = 200;
/// Characters of the project's AGENTS.md or CLAUDE.md a project reports at most.
pub const MAX_CONVENTIONS_CHARS: usize = 2_000;
pub const MAX_TITLE_CHARS: usize = 200;
const MAX_REF_CHARS: usize = 256;

/// `POST /v1/agent/dispatch-inputs`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchInputs {
    pub device: DeviceId,
    /// Herdr answered on this device.
    pub herdr: bool,
    pub repos: Vec<RepoReport>,
}

/// One git repository on the device.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoReport {
    /// The repository's main working tree, absolute.
    pub path: String,
    /// Its last path component.
    pub name: String,
    /// owner/name of the origin remote on GitHub.
    pub slug: Option<String>,
    /// The local branch origin/HEAD names.
    pub default_branch: Option<String>,
    /// An owner agent session is working here now.
    pub working: bool,
    // Filled only for projects the coordinator said are on:
    pub base_commit: Option<String>,
    pub roadmap: Vec<RoadmapItem>,
    /// The owner's changed paths.
    pub touched: Vec<String>,
    /// More than `MAX_TOUCHED` paths changed; the list is cut.
    pub touched_clipped: bool,
    /// The files tracked at the base commit, at most `MAX_FILES`.
    pub files: Vec<String>,
    /// How many tracked files the list left out.
    pub files_cut: usize,
    /// The start of the project's AGENTS.md (else CLAUDE.md) at the base commit, so a pass
    /// follows its conventions for new files.
    pub conventions: Option<String>,
    pub checkout: Option<Checkout>,
}

/// The project's base checkout under `$IDLEMAX_HOME/checks`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Checkout {
    pub path: String,
    pub commit: Option<String>,
    pub error: Option<String>,
}

/// The reply: this device's switched-on projects.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchReply {
    pub projects: Vec<Project>,
}

/// One repository of a device's newest feed, as the store returns it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredRepo {
    pub device: DeviceId,
    pub received_ms: UnixMs,
    pub repo: RepoReport,
}

impl DispatchInputs {
    /// The first reason this feed must be refused for its size or characters, if any.
    pub fn check(&self) -> Result<(), String> {
        problem(name_problem("device", &self.device, MAX_DEVICE_ID_CHARS))?;
        if self.repos.len() > MAX_REPOS {
            return Err(format!("repos has more than {MAX_REPOS} entries"));
        }
        self.repos.iter().try_for_each(RepoReport::check)
    }
}

impl RepoReport {
    fn check(&self) -> Result<(), String> {
        let texts = [
            ("path", Some(self.path.as_str()), MAX_PATH_CHARS),
            ("name", Some(self.name.as_str()), MAX_NAME_CHARS),
            ("slug", self.slug.as_deref(), MAX_NAME_CHARS),
            (
                "default_branch",
                self.default_branch.as_deref(),
                MAX_REF_CHARS,
            ),
            ("base_commit", self.base_commit.as_deref(), MAX_REF_CHARS),
        ];
        for (field, value, max) in texts {
            if let Some(v) = value {
                problem(name_problem(field, v, max))?;
            }
        }
        if self.roadmap.len() > MAX_ROADMAP_ITEMS {
            return Err(format!("roadmap has more than {MAX_ROADMAP_ITEMS} items"));
        }
        for item in &self.roadmap {
            problem(name_problem("roadmap title", &item.title, MAX_TITLE_CHARS))?;
            let done_when = &item.done_when;
            problem(name_problem(
                "roadmap done_when",
                done_when,
                MAX_DONE_WHEN_CHARS,
            ))?;
            problem(name_problem(
                "roadmap detail",
                &item.detail,
                super::roadmap::MAX_DETAIL_CHARS,
            ))?;
        }
        if self.touched.len() > MAX_TOUCHED {
            return Err(format!("touched has more than {MAX_TOUCHED} paths"));
        }
        for path in &self.touched {
            problem(name_problem("touched path", path, MAX_PATH_CHARS))?;
        }
        if self.files.len() > MAX_FILES {
            return Err(format!("files has more than {MAX_FILES} paths"));
        }
        for path in &self.files {
            problem(name_problem("file path", path, MAX_PATH_CHARS))?;
        }
        if let Some(c) = &self.conventions {
            if c.chars().count() > MAX_CONVENTIONS_CHARS {
                return Err(format!(
                    "conventions over {MAX_CONVENTIONS_CHARS} characters"
                ));
            }
            if c.chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
            {
                return Err("conventions hold a control character".into());
            }
        }
        if let Some(c) = &self.checkout {
            problem(name_problem("checkout path", &c.path, MAX_PATH_CHARS))?;
        }
        Ok(())
    }
}

fn problem(p: Option<String>) -> Result<(), String> {
    p.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(repo: RepoReport, repos: usize) -> DispatchInputs {
        DispatchInputs {
            device: "mac".into(),
            herdr: true,
            repos: vec![repo; repos],
        }
    }

    #[test]
    fn the_feed_is_bounded_and_plain() {
        let repo = RepoReport {
            path: "/a/app".into(),
            name: "app".into(),
            ..Default::default()
        };
        assert_eq!(feed(repo.clone(), 64).check(), Ok(()));
        assert!(feed(repo.clone(), 65).check().is_err());
        let many = RepoReport {
            touched: vec!["a".into(); 201],
            ..repo.clone()
        };
        assert!(feed(many, 1).check().is_err());
        let escape = RepoReport {
            roadmap: vec![RoadmapItem {
                title: "x\u{1b}[31m".into(),
                ..Default::default()
            }],
            ..repo
        };
        assert!(feed(escape, 1).check().is_err());
    }
}
