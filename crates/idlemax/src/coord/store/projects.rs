//! Project rows (adapter): the repositories idlemax knows about, off until the owner switches
//! one on, with the owner's settings as JSON. The domain checks names and settings.

use idlemax_core::dispatch::inputs::RepoReport;
use idlemax_core::dispatch::project::{
    NewProject, Project, ProjectSettings, ProjectSource, ProjectView,
};
use idlemax_core::types::UnixMs;
use rusqlite::{params, OptionalExtension, Row};

use super::{enum_col, enum_text, json_col, json_text, Store, StoreError, StoreResult};

/// Most projects one device may hold.
pub const MAX_PROJECTS_PER_DEVICE: i64 = 200;

const COLUMNS: &str = "id, device, name, path, enabled, settings, source, seen_ms, owner_active_ms";

impl Store {
    /// By device, then name; `device` keeps one device.
    pub fn list_projects(&self, device: Option<&str>) -> StoreResult<Vec<ProjectView>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM projects WHERE ?1 IS NULL OR device = ?1 ORDER BY device, name"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([device], project_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_project(&self, id: i64) -> StoreResult<Option<ProjectView>> {
        let sql = format!("SELECT {COLUMNS} FROM projects WHERE id = ?1");
        Ok(self
            .conn
            .query_row(&sql, [id], project_from_row)
            .optional()?)
    }

    /// Adds a project that is off and returns it with true, or the known row of the same
    /// device and path with false. A taken name gets `-2`, `-3` and so on.
    pub fn insert_project(
        &mut self,
        new: &NewProject,
        device: &str,
        source: ProjectSource,
        now: UnixMs,
    ) -> StoreResult<(ProjectView, bool)> {
        new.check().map_err(StoreError::Invalid)?;
        let tx = self.conn.transaction()?;
        let known: Option<i64> = tx
            .query_row(
                "SELECT id FROM projects WHERE device = ?1 AND path = ?2",
                params![device, new.path],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = known {
            drop(tx);
            let view = self.get_project(id)?.ok_or_else(|| gone(id))?;
            return Ok((view, false));
        }
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM projects WHERE device = ?1",
            [device],
            |r| r.get(0),
        )?;
        if count >= MAX_PROJECTS_PER_DEVICE {
            return Err(StoreError::LimitReached {
                what: "projects on one device",
                limit: MAX_PROJECTS_PER_DEVICE,
                hint: "a project that is off and was not seen for 30 days is forgotten",
            });
        }
        let base = new.name();
        let mut name = base.clone();
        for n in 2.. {
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE device = ?1 AND name = ?2)",
                params![device, name],
                |r| r.get(0),
            )?;
            if !taken {
                break;
            }
            name = format!("{base}-{n}");
        }
        tx.execute(
            "INSERT INTO projects (device, name, path, source, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![device, name, new.path, enum_text(&source)?, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        let view = self.get_project(id)?.ok_or_else(|| gone(id))?;
        Ok((view, true))
    }

    /// Returns the updated project, or None when the id is unknown.
    pub fn set_project_enabled(&mut self, id: i64, on: bool) -> StoreResult<Option<ProjectView>> {
        self.conn.execute(
            "UPDATE projects SET enabled = ?2 WHERE id = ?1",
            params![id, on],
        )?;
        self.get_project(id)
    }

    /// Returns the updated project, or None when the id is unknown.
    pub fn set_project_settings(
        &mut self,
        id: i64,
        settings: &ProjectSettings,
    ) -> StoreResult<Option<ProjectView>> {
        settings.validate().map_err(StoreError::Invalid)?;
        self.conn.execute(
            "UPDATE projects SET settings = ?2 WHERE id = ?1",
            params![id, json_text(settings)?],
        )?;
        self.get_project(id)
    }
}

impl Store {
    /// Records a device's feed: an unknown repository becomes a project that is off, with
    /// its default branch as the base; a known one is marked seen, and owner-active when an
    /// owner session works there. A repository over the device's limit is skipped, so the
    /// limit never fails a feed.
    pub fn upsert_discovered(
        &mut self,
        device: &str,
        repos: &[RepoReport],
        now: UnixMs,
    ) -> StoreResult<()> {
        for repo in repos {
            let new = NewProject {
                path: repo.path.clone(),
                device: Some(device.to_string()),
                name: Some(repo.name.clone()).filter(|n| !n.trim().is_empty()),
            };
            let (view, created) = match self.insert_project(&new, device, ProjectSource::Herdr, now)
            {
                Ok(found) => found,
                Err(StoreError::LimitReached { .. } | StoreError::Invalid(_)) => continue,
                Err(e) => return Err(e),
            };
            if created {
                let settings = ProjectSettings {
                    base_branch: repo.default_branch.clone(),
                    ..ProjectSettings::default()
                };
                if settings.validate().is_ok() {
                    self.set_project_settings(view.project.id, &settings)?;
                }
            }
            self.conn.execute(
                "UPDATE projects SET seen_ms = ?2,
                   owner_active_ms = CASE WHEN ?3 THEN ?2 ELSE owner_active_ms END
                 WHERE id = ?1",
                params![view.project.id, now, repo.working],
            )?;
        }
        Ok(())
    }
}

fn gone(id: i64) -> StoreError {
    StoreError::Corrupt(format!("project {id} vanished"))
}

fn project_from_row(r: &Row<'_>) -> rusqlite::Result<ProjectView> {
    let settings: ProjectSettings = json_col(r, 5)?;
    Ok(ProjectView {
        project: Project {
            id: r.get(0)?,
            device: r.get(1)?,
            name: r.get(2)?,
            path: r.get(3)?,
            enabled: r.get(4)?,
            settings,
        },
        source: enum_col(r, 6)?,
        seen_ms: r.get(7)?,
        owner_active_ms: r.get(8)?,
        owner_active: false,
        warning: None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::schema;
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn new(path: &str) -> NewProject {
        NewProject {
            path: path.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_v7_store_migrates_and_keeps_its_devices() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for sql in &schema::MIGRATIONS[..7] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute(
            "INSERT INTO devices (id, info, first_seen_ms, last_seen_ms) VALUES ('mac', '{}', 1, 2)",
            [],
        )
        .unwrap();
        let s = Store::init(conn, false).unwrap();
        let (id, room): (String, Option<String>) = s
            .conn
            .query_row("SELECT id, room FROM devices", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((id.as_str(), room), ("mac", None));
        let version: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::MIGRATIONS.len() as i64);
    }

    #[test]
    fn a_known_path_returns_its_row_and_a_taken_name_gets_a_suffix() {
        let mut s = Store::open_in_memory().unwrap();
        let (first, created) = s
            .insert_project(&new("/a/app"), "mac", ProjectSource::Owner, T0)
            .unwrap();
        assert!(created && !first.project.enabled);
        let (again, created) = s
            .insert_project(&new("/a/app"), "mac", ProjectSource::Herdr, T0)
            .unwrap();
        assert!(!created);
        assert_eq!(again.project.id, first.project.id);
        let (other, _) = s
            .insert_project(&new("/b/app"), "mac", ProjectSource::Owner, T0)
            .unwrap();
        assert_eq!(other.project.name, "app-2");
        let on = s.set_project_enabled(first.project.id, true).unwrap();
        assert!(on.unwrap().project.enabled);
        assert_eq!(s.list_projects(Some("mac")).unwrap().len(), 2);
        assert!(s.list_projects(Some("srv")).unwrap().is_empty());
    }
}
