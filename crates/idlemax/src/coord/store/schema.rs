//! Store schema migrations (adapter). Append new migrations; never edit applied ones.

/// Index i holds the SQL that moves `PRAGMA user_version` from i to i + 1.
pub(super) const MIGRATIONS: &[&str] = &[V1, V2, V3, V4];

const V1: &str = r#"
CREATE TABLE devices (
  id TEXT PRIMARY KEY,
  info TEXT NOT NULL,
  first_seen_ms INTEGER NOT NULL,
  last_seen_ms INTEGER NOT NULL,
  metrics TEXT,
  guard TEXT,
  running TEXT NOT NULL DEFAULT '[]',
  interactive_use TEXT NOT NULL DEFAULT '[]',
  local_paused INTEGER NOT NULL DEFAULT 0
) STRICT;

-- 30 s heartbeats folded into one row per device per minute.
CREATE TABLE metrics_minute (
  device TEXT NOT NULL,
  minute_ms INTEGER NOT NULL,
  samples INTEGER NOT NULL,
  cpu_busy_pct_avg REAL NOT NULL,
  load_avg_1m_max REAL NOT NULL,
  mem_free_pct_min REAL NOT NULL,
  mem_available_bytes_min INTEGER NOT NULL,
  swap_used_bytes_max INTEGER NOT NULL,
  disk_free_bytes_min INTEGER NOT NULL,
  user_idle_secs_min INTEGER,
  on_ac_power INTEGER,
  running_jobs_max INTEGER NOT NULL,
  guard_severity_max INTEGER NOT NULL,
  PRIMARY KEY (device, minute_ms)
) STRICT, WITHOUT ROWID;
CREATE INDEX metrics_minute_age ON metrics_minute(minute_ms);

-- A lease is the device plus lease_expires_ms of a leased, running or paused job.
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  spec TEXT NOT NULL,
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'leased', 'running', 'paused',
    'succeeded', 'failed', 'killed', 'cancelled', 'parked')),
  priority INTEGER NOT NULL,
  ai_account TEXT,
  device TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_ms INTEGER NOT NULL,
  started_ms INTEGER,
  finished_ms INTEGER,
  lease_expires_ms INTEGER,
  not_before_ms INTEGER,
  exit_code INTEGER,
  last_error TEXT,
  output_path TEXT
) STRICT;
CREATE INDEX jobs_by_status ON jobs(status, priority DESC, id);
-- At most one AI lease per account in the whole cluster (account ban guard).
CREATE UNIQUE INDEX jobs_one_ai_lease ON jobs(ai_account)
  WHERE ai_account IS NOT NULL AND status IN ('leased', 'running', 'paused');

CREATE TABLE quota_snapshots (
  account TEXT NOT NULL,
  quota_window TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  used_pct REAL NOT NULL,
  resets_at_ms INTEGER,
  source TEXT NOT NULL,
  PRIMARY KEY (account, quota_window, at_ms)
) STRICT, WITHOUT ROWID;

-- Newest usage limit or HTTP 429 seen per AI account.
CREATE TABLE rate_limits (
  account TEXT PRIMARY KEY,
  at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- A row means that scope is paused by a coordinator control.
CREATE TABLE controls (
  scope_type TEXT NOT NULL CHECK (scope_type IN ('global', 'device', 'account', 'job')),
  scope_id TEXT NOT NULL,
  PRIMARY KEY (scope_type, scope_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  device TEXT,
  job INTEGER,
  message TEXT NOT NULL
) STRICT;
CREATE INDEX events_age ON events(at_ms);

CREATE TABLE recommendations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at_ms INTEGER NOT NULL,
  key TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  why TEXT NOT NULL,
  evidence TEXT NOT NULL,
  suggested_job TEXT,
  confidence REAL NOT NULL,
  source TEXT NOT NULL,
  status TEXT NOT NULL
) STRICT;
CREATE INDEX recommendations_by_status ON recommendations(status, at_ms);
"#;

// Newest advisor inputs (GitHub summary, session metadata) each agent sent, as JSON.
const V2: &str = r#"
CREATE TABLE advisor_inputs (
  device TEXT PRIMARY KEY,
  received_ms INTEGER NOT NULL,
  payload TEXT NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Agent job updates already applied, so a replayed update changes nothing twice.
const V3: &str = r#"
CREATE TABLE applied_updates (
  device TEXT NOT NULL,
  seq INTEGER NOT NULL,
  at_ms INTEGER NOT NULL,
  PRIMARY KEY (device, seq)
) STRICT, WITHOUT ROWID;
CREATE INDEX applied_updates_age ON applied_updates(at_ms);
"#;

/// Existing devices start offline, so an upgrade never repeats an old offline event.
const V4: &str = r#"
ALTER TABLE devices ADD COLUMN online INTEGER NOT NULL DEFAULT 0;
"#;
