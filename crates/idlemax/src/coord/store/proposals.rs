//! Rule proposal rows (adapter): the lines the orchestrator proposed for `rules.md`, and the
//! finished runs a pass may propose one for. The domain decides which proposals are kept.

use idlemax_core::dispatch::proposal::{
    FinishedRun, Proposal, ProposalStatus, RuleProposal, MAX_FINISHED,
};
use idlemax_core::types::UnixMs;
use rusqlite::{params, OptionalExtension, Row};

use super::{enum_col, enum_text, Store, StoreResult};

const COLUMNS: &str = "id, at_ms, run, line, reason, status, decided_ms";

impl Store {
    /// Every stored proposal, open ones first, then newest first. Bounded by `prune`.
    pub fn proposals(&self) -> StoreResult<Vec<Proposal>> {
        let sql =
            format!("SELECT {COLUMNS} FROM rule_proposals ORDER BY status = 'open' DESC, id DESC");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], proposal_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn proposal(&self, id: i64) -> StoreResult<Option<Proposal>> {
        let sql = format!("SELECT {COLUMNS} FROM rule_proposals WHERE id = ?1");
        Ok(self
            .conn
            .query_row(&sql, [id], proposal_from_row)
            .optional()?)
    }

    pub fn open_proposals(&self) -> StoreResult<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM rule_proposals WHERE status = 'open'",
            [],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub fn insert_proposal(&mut self, p: &RuleProposal, now: UnixMs) -> StoreResult<Proposal> {
        self.conn.execute(
            "INSERT INTO rule_proposals (at_ms, run, line, reason, status)
             VALUES (?1, ?2, ?3, ?4, 'open')",
            params![now, p.run, p.line, p.reason],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Proposal {
            id,
            at_ms: now,
            run: p.run,
            line: p.line.clone(),
            reason: p.reason.clone(),
            status: ProposalStatus::Open,
            decided_ms: None,
        })
    }

    /// Approves or rejects an open proposal; false when it was not open.
    pub fn decide_proposal(
        &mut self,
        id: i64,
        status: ProposalStatus,
        now: UnixMs,
    ) -> StoreResult<bool> {
        let changed = self.conn.execute(
            "UPDATE rule_proposals SET status = ?2, decided_ms = ?3
             WHERE id = ?1 AND status = 'open'",
            params![id, enum_text(&status)?, now],
        )?;
        Ok(changed == 1)
    }

    /// The newest rejected or discarded runs that have no proposal yet.
    pub fn finished_runs(&self) -> StoreResult<Vec<FinishedRun>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, project, task_title, state, concern FROM runs
             WHERE state IN ('rejected', 'discarded')
               AND id NOT IN (SELECT run FROM rule_proposals)
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([MAX_FINISHED], |r| {
            Ok(FinishedRun {
                run: r.get(0)?,
                project: r.get(1)?,
                title: r.get(2)?,
                state: enum_col(r, 3)?,
                concern: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn proposal_from_row(r: &Row<'_>) -> rusqlite::Result<Proposal> {
    Ok(Proposal {
        id: r.get(0)?,
        at_ms: r.get(1)?,
        run: r.get(2)?,
        line: r.get(3)?,
        reason: r.get(4)?,
        status: enum_col(r, 5)?,
        decided_ms: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::super::schema;
    use super::*;

    const T0: UnixMs = 1_800_000_000_000;

    fn add_run(conn: &Connection, id: i64, state: &str) {
        conn.execute(
            "INSERT INTO runs (id, device, project, task, task_title, source, provider,
               account, model, state, reason, launch, start_ms, updated_ms)
             VALUES (?1, 'mac', 'app', 't', 'Fix it', 'roadmap', 'claude', 'max', 'sonnet',
               ?2, 'why', '{}', ?3, ?3)",
            params![id, state, T0],
        )
        .unwrap();
    }

    fn rule(run: i64, line: &str) -> RuleProposal {
        RuleProposal {
            run,
            line: line.into(),
            reason: "it broke".into(),
        }
    }

    #[test]
    fn proposals_list_open_first_decide_once_and_leave_the_finished_list() {
        let mut s = Store::open_in_memory().unwrap();
        for (id, state) in [(1, "rejected"), (2, "discarded"), (3, "clean")] {
            add_run(&s.conn, id, state);
        }
        let runs: Vec<i64> = s.finished_runs().unwrap().iter().map(|f| f.run).collect();
        assert_eq!(runs, [2, 1]);

        let first = s.insert_proposal(&rule(1, "One."), T0).unwrap();
        let second = s.insert_proposal(&rule(2, "Two."), T0).unwrap();
        assert!(s.finished_runs().unwrap().is_empty());
        assert!(s
            .decide_proposal(first.id, ProposalStatus::Rejected, T0 + 1)
            .unwrap());
        assert!(!s
            .decide_proposal(first.id, ProposalStatus::Approved, T0 + 2)
            .unwrap());

        let all = s.proposals().unwrap();
        assert_eq!(
            all.iter().map(|p| p.id).collect::<Vec<_>>(),
            [second.id, first.id]
        );
        assert_eq!(all[1].status, ProposalStatus::Rejected);
        assert_eq!(all[1].decided_ms, Some(T0 + 1));
        assert_eq!(s.open_proposals().unwrap(), 1);
    }

    #[test]
    fn a_v10_store_migrates_and_keeps_its_runs() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &schema::MIGRATIONS[..10] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 10).unwrap();
        add_run(&conn, 1, "rejected");
        let s = Store::init(conn, false).unwrap();
        let version: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::MIGRATIONS.len() as i64);
        assert_eq!(s.finished_runs().unwrap().len(), 1);
    }
}
