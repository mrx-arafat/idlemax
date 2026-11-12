//! Store schema migrations (adapter). Append new migrations; never edit applied ones.

/// Index i holds the SQL that moves `PRAGMA user_version` from i to i + 1.
pub(super) const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6, V7, V8, V9];

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

/// Recurring jobs. `last_job_id` points at the job the last fire queued; jobs are never
/// deleted, so it needs no foreign key.
const V5: &str = r#"
CREATE TABLE schedules (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  spec TEXT NOT NULL,
  every_secs INTEGER NOT NULL CHECK (every_secs > 0),
  paused INTEGER NOT NULL DEFAULT 0,
  created_ms INTEGER NOT NULL,
  last_fired_ms INTEGER,
  last_job_id INTEGER
) STRICT;
"#;

/// Idempotency key of the job spec (`JobSpec.key`), so a retried add finds its unfinished job.
/// Older rows have none.
const V6: &str = r#"
ALTER TABLE jobs ADD COLUMN key TEXT;
CREATE INDEX jobs_by_key ON jobs(key, status) WHERE key IS NOT NULL;
"#;

/// The guard verdict of each device as spans, and the coordinator's pauses as spans, for the
/// value report. A guard row is written when the verdict changes; each heartbeat that repeats it moves `end_ms` on, so the table grows with
/// changes, not with samples. A device's newest span is its highest `id`, never its latest
/// `start_ms`, so a clock that steps back cannot start a row per heartbeat. `reasons` is the
/// stored names of the hold reasons, comma separated.
const V7: &str = r#"
CREATE TABLE guard_spans (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  held INTEGER NOT NULL,
  reasons TEXT NOT NULL
) STRICT;
CREATE INDEX guard_spans_newest ON guard_spans(device, id);
-- The span that began last before a report's start, found per device without a scan.
CREATE INDEX guard_spans_by_start ON guard_spans(device, start_ms);
CREATE INDEX guard_spans_age ON guard_spans(end_ms);
-- The report reads the lifecycle events of jobs.
CREATE INDEX events_by_job ON events(job) WHERE job IS NOT NULL;

-- Coordinator pauses as spans, so a report can tell a pause from a device with room to spare.
-- A row is opened when a scope is paused and closed when it is resumed.
CREATE TABLE pause_spans (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  scope_type TEXT NOT NULL CHECK (scope_type IN ('global', 'device', 'account', 'job')),
  scope_id TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER
) STRICT;
CREATE INDEX pause_spans_open ON pause_spans(scope_type, scope_id) WHERE end_ms IS NULL;
CREATE INDEX pause_spans_age ON pause_spans(end_ms);
-- Pauses already in force when this version first runs start now.
INSERT INTO pause_spans (scope_type, scope_id, start_ms)
  SELECT scope_type, scope_id, CAST(strftime('%s', 'now') AS INTEGER) * 1000 FROM controls;
"#;

/// The dispatcher (plan only): each device's worker room, projects, the newest dispatch inputs
/// per device, base checks, orchestrator passes and the tasks the orchestrator sent to
/// "needs you". One migration for the whole of dispatcher Spec 1.
const V8: &str = r#"
-- The worker room each agent reported last, as JSON; NULL from an agent that predates it.
ALTER TABLE devices ADD COLUMN room TEXT;
-- Git repositories idlemax knows about. Off until the owner switches one on.
CREATE TABLE projects (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device TEXT NOT NULL,
  name TEXT NOT NULL,
  path TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 0,
  settings TEXT NOT NULL DEFAULT '{}',
  source TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  seen_ms INTEGER,
  owner_active_ms INTEGER,
  UNIQUE (device, path),
  UNIQUE (device, name)
) STRICT;
-- Newest dispatch inputs per device, as JSON.
CREATE TABLE dispatch_inputs (
  device TEXT PRIMARY KEY,
  received_ms INTEGER NOT NULL,
  payload TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- The current run of each project's focused check on its base branch.
CREATE TABLE base_checks (
  project INTEGER PRIMARY KEY,
  base_commit TEXT NOT NULL,
  phase TEXT NOT NULL,
  job INTEGER,
  passed INTEGER,
  detail TEXT,
  finished_ms INTEGER
) STRICT;
-- Orchestrator passes: what each one was offered, saw and returned.
CREATE TABLE orchestrator_passes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job INTEGER NOT NULL,
  account TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  offered TEXT NOT NULL,
  queued_ms INTEGER NOT NULL,
  finished_ms INTEGER,
  result TEXT,
  error TEXT
) STRICT;
CREATE INDEX orchestrator_passes_newest ON orchestrator_passes(queued_ms);
-- Tasks the orchestrator sent to "needs you"; a row lasts until its task changes (A20).
CREATE TABLE task_judgments (
  task TEXT PRIMARY KEY,
  digest TEXT NOT NULL,
  reason TEXT NOT NULL,
  judged_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Dispatcher workers: the Run of each worker, the owner's stop, and worker jobs marked so the
/// one-AI-lease rule leaves them out (A8).
const V9: &str = r#"
ALTER TABLE jobs ADD COLUMN worker INTEGER NOT NULL DEFAULT 0;
DROP INDEX jobs_one_ai_lease;
-- At most one AI lease per account for plain jobs; worker jobs stand outside the rule.
CREATE UNIQUE INDEX jobs_one_ai_lease ON jobs(ai_account)
  WHERE ai_account IS NOT NULL AND status IN ('leased', 'running', 'paused') AND worker = 0;
-- One worker on one task, from its first launch to the owner's review.
CREATE TABLE runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device TEXT NOT NULL,
  project TEXT NOT NULL,
  task TEXT NOT NULL,
  task_title TEXT NOT NULL,
  source TEXT NOT NULL,
  branch TEXT,
  worktree TEXT,
  provider TEXT NOT NULL,
  account TEXT NOT NULL,
  model TEXT NOT NULL,
  session TEXT,
  state TEXT NOT NULL CHECK (state IN ('starting', 'running', 'checking', 'clean',
    'needs_you', 'rejected', 'stopped', 'shed', 'kept', 'discarded')),
  verdict TEXT CHECK (verdict IS NULL OR verdict IN ('clean', 'needs_you', 'rejected')),
  concern TEXT,
  reason TEXT NOT NULL,
  summary TEXT,
  -- What a relaunch needs: the worker section and the brief, as JSON. Never sent out.
  launch TEXT NOT NULL,
  facts TEXT NOT NULL DEFAULT '{}',
  quota_before TEXT NOT NULL DEFAULT '[]',
  quota_after TEXT NOT NULL DEFAULT '[]',
  job INTEGER,
  launches INTEGER NOT NULL DEFAULT 0,
  start_tries INTEGER NOT NULL DEFAULT 0,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER,
  updated_ms INTEGER NOT NULL
) STRICT;
CREATE INDEX runs_by_state ON runs(state, device);
CREATE INDEX runs_by_project ON runs(project, state);
CREATE INDEX runs_by_account ON runs(account, start_ms);
-- The owner's stop of every worker: at most one row, kept until the owner's start.
CREATE TABLE worker_stop (
  one INTEGER PRIMARY KEY CHECK (one = 1),
  stopped_ms INTEGER NOT NULL,
  reason TEXT NOT NULL
) STRICT;
"#;
