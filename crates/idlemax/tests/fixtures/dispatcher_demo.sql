-- Seed rows for the dispatcher view: the route test, the browser scenarios and the
-- screenshots of dispatcher Spec 4. One device that went offline two hours ago, two projects
-- (app on, site off), four runs with a job each, and a roadmap item of app that says it needs
-- the owner. Times are relative to the load and every lease expiry lies a day ahead, so no
-- sweep touches a seeded job. Load it into a store that the coordinator created.

INSERT INTO devices (id, info, first_seen_ms, last_seen_ms) VALUES
  ('mac', '{"id":"mac","hostname":"mac"}',
   strftime('%s','now') * 1000 - 86400000, strftime('%s','now') * 1000 - 7200000);

INSERT INTO projects (id, device, name, path, enabled, settings, source, created_ms) VALUES
  (1, 'mac', 'app', '/srv/app', 1,
   '{"base_branch":"main","check_command":["cargo","test"]}', 'owner',
   strftime('%s','now') * 1000 - 86400000),
  (2, 'mac', 'site', '/srv/site', 0, '{}', 'owner', strftime('%s','now') * 1000 - 86400000);

INSERT INTO dispatch_inputs (device, received_ms, payload) VALUES
  ('mac', strftime('%s','now') * 1000 - 7200000,
   '{"device":"mac","herdr":true,"repos":[{"path":"/srv/app","name":"app",'
   || '"default_branch":"main","roadmap":[{"title":"Pick the release date",'
   || '"done_when":"the date is in ROADMAP.md","done":false,"owner":true}]}]}');

INSERT INTO jobs (id, spec, name, status, priority, ai_account, device, created_ms, started_ms,
                  finished_ms, lease_expires_ms, worker) VALUES
  (1, '{"name":"worker:app:1","command":["true"]}', 'worker:app:1', 'running', 0, 'max', 'mac',
   strftime('%s','now') * 1000 - 1800000, strftime('%s','now') * 1000 - 1800000, NULL,
   strftime('%s','now') * 1000 + 86400000, 1),
  (2, '{"name":"worker:app:2","command":["true"]}', 'worker:app:2', 'running', 0, 'max', 'mac',
   strftime('%s','now') * 1000 - 3600000, strftime('%s','now') * 1000 - 3600000, NULL,
   strftime('%s','now') * 1000 + 86400000, 1),
  (3, '{"name":"worker:app:3","command":["true"]}', 'worker:app:3', 'succeeded', 0, 'max', 'mac',
   strftime('%s','now') * 1000 - 7200000, strftime('%s','now') * 1000 - 7200000,
   strftime('%s','now') * 1000 - 5400000, NULL, 1),
  (4, '{"name":"worker:app:4","command":["true"]}', 'worker:app:4', 'succeeded', 0, 'max', 'mac',
   strftime('%s','now') * 1000 - 9000000, strftime('%s','now') * 1000 - 9000000,
   strftime('%s','now') * 1000 - 7200000, NULL, 1);

INSERT INTO runs (id, device, project, task, task_title, source, branch, provider, account,
                  model, state, verdict, concern, reason, launch, job, launches, start_ms,
                  end_ms, updated_ms) VALUES
  (1, 'mac', 'app', 'roadmap:1:add-dark-mode', 'Add dark mode', 'roadmap',
   'idlemax/add-dark-mode', 'claude', 'max', 'sonnet', 'running', NULL, NULL,
   'first roadmap item; the check is focused', '{}', 1, 1,
   strftime('%s','now') * 1000 - 1800000, NULL, strftime('%s','now') * 1000 - 1800000),
  (2, 'mac', 'app', 'roadmap:1:export-csv', 'Export as CSV', 'roadmap',
   'idlemax/export-csv', 'claude', 'max', 'sonnet', 'checking', NULL, NULL,
   'small and covered by tests', '{}', 2, 1,
   strftime('%s','now') * 1000 - 3600000, NULL, strftime('%s','now') * 1000 - 600000),
  (3, 'mac', 'app', 'roadmap:1:fix-login', 'Fix the login test', 'roadmap',
   'idlemax/fix-login', 'claude', 'max', 'sonnet', 'clean', 'clean', NULL,
   'the base check fails on it', '{}', 3, 1,
   strftime('%s','now') * 1000 - 7200000, strftime('%s','now') * 1000 - 5400000,
   strftime('%s','now') * 1000 - 5400000),
  (4, 'mac', 'app', 'roadmap:1:speed-up-build', 'Speed up the build', 'roadmap',
   'idlemax/speed-up-build', 'claude', 'max', 'sonnet', 'needs_you', 'needs_you',
   'it changes the release script', 'the build takes 9 minutes', '{}', 4, 1,
   strftime('%s','now') * 1000 - 9000000, strftime('%s','now') * 1000 - 7200000,
   strftime('%s','now') * 1000 - 7200000);
