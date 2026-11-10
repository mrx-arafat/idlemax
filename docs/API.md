# idlemax HTTP API

Types live in `crates/idlemax-core/src/{types,protocol,quota}.rs`; this page describes the wire format and the semantics around them.
All bodies are JSON.
Every serde enum is `snake_case`.
Optional fields may be omitted on input; missing fields take their defaults.

## Auth and errors

Every call except `GET /healthz`, `GET /` and `GET /dashboard.js` needs `Authorization: Bearer <token>`.
The token comes from `IDLEMAX_TOKEN` or `$IDLEMAX_HOME/token` (mode 0600) and must be at least 32 characters; `idlemax init` makes 64.
Any non-2xx response has the body `{"error": "..."}` (`protocol::ApiError`).

Every route, token-free ones included, also checks the `Host` header (DNS-rebinding defence).
It must be an IP address, `localhost` or a `*.localhost` name, the host of `coordinator.url`, or a name in `coordinator.allowed_hosts`; any other name gets `421 Misdirected Request`.
A request with no `Host` header is served.
The agent local API accepts IP addresses and `localhost` names only.

## Coordinator (default `127.0.0.1:7878`, tailnet IP in production)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | /healthz | | `ok` (text, no auth) |
| GET | / | | dashboard HTML (no auth) |
| GET | /dashboard.js | | the dashboard's one script, `text/javascript` (no auth) |
| POST | /v1/agent/heartbeat | `Heartbeat` | `HeartbeatReply` |
| POST | /v1/agent/job-update | `JobUpdate` | `JobUpdateReply` |
| POST | /v1/agent/advisor-inputs | `AdvisorInputs` | `{}` |
| GET | /v1/status | | `ClusterStatus` |
| GET | /v1/jobs?status=&limit= | | `[Job]` |
| GET | /v1/jobs/{id} | | `Job` |
| POST | /v1/jobs | `JobSpec` | `Job` |
| POST | /v1/jobs/{id}/cancel | | `Job` |
| GET | /v1/jobs/{id}/why | | `Explanation` |
| GET | /v1/why?limit= | | `[WhySummary]` |
| GET | /v1/quota | | `[quota::QuotaView]` |
| POST | /v1/control | `ControlRequest` | `ClusterStatus` |
| GET | /v1/events?limit= | | `[Event]` |
| GET | /v1/advice | | `[Recommendation]`, open ones first |
| POST | /v1/advice/refresh | | `[Recommendation]` after re-running the advisor |
| POST | /v1/advice/{id}/accept | | `Job` queued from the suggested job |
| POST | /v1/advice/{id}/dismiss | | `Recommendation` |
| GET | /v1/schedules | | `[ScheduleView]`, oldest first |
| POST | /v1/schedules | `NewSchedule` | `ScheduleView` |
| DELETE | /v1/schedules/{id} | | `ScheduleView` as it was before removal |
| POST | /v1/schedules/{id}/pause | | `ScheduleView` |
| POST | /v1/schedules/{id}/resume | | `ScheduleView` |

`status` is a `JobStatus` (`queued`, `leased`, `running`, `paused`, `succeeded`, `failed`, `killed`, `cancelled`, `parked`).
`POST /v1/jobs` returns 201 with the queued job; it answers 400 for a spec that fails `JobSpec::validate` or names an `ai_account` missing from `[[quota.accounts]]`, and 409 when 10000 jobs are already unfinished.
`JobSpec::validate` bounds every field, and the 400 names the field and its limit:

| Field | Limit |
|---|---|
| `name` | 200 characters, no control characters |
| `command` | 1024 arguments, 128 KiB each, 512 KiB in all, no NUL byte |
| `env` | 256 variables, names without `=`, values up to 32 KiB, 256 KiB in all |
| `stdin` | 1 MiB |
| `cwd` | 4096 bytes, no NUL byte |
| `requires`, `devices` | 64 entries of up to 128 characters |
| `ai_account` | 128 characters |
A spec may carry `key`, an idempotency key of 1 to 128 characters.
While a job with that key is queued, leased, running or paused, `POST /v1/jobs` queues nothing and answers 200 with that job, so a client may retry safely.
Once the job has finished the key is free again.
The first spec wins: a retry with a changed spec still returns the original job.
An unset `key` is left out of the JSON, and a coordinator or agent that predates it ignores the field.
`POST /v1/jobs/{id}/cancel` answers 404 for an unknown job and 409 for a finished one.
`GET /v1/jobs/{id}/why` answers 404 for an unknown job (see [Why a job is not running](#why-a-job-is-not-running)).
A job update for an unknown job is 404.
`coordinator.listen` on `0.0.0.0`, `[::]` or `[::ffff:0.0.0.0]` is refused at startup.

## Agent local API (default `127.0.0.1:7879`)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | /healthz | | `ok` (no auth) |
| GET | /v1/local/status | | `LocalStatus` |
| POST | /v1/local/control | `ControlRequest` with `global` scope; `pause`, `resume` or `stop_all` | `LocalStatus` |
| POST | /v1/local/reload | | `ReloadReport` |

It keeps working while the coordinator is down.
`POST /v1/local/control` waits up to 5 s for `local_paused` to reflect the action, so the agent publishes status through a `tokio::sync::watch` channel.
It returns 200 once applied, 202 when the agent has not applied it within 5 s, 400 for a non-global scope or `cancel`, and 503 when the agent loop is gone.
Creating `$IDLEMAX_HOME/PAUSE` pauses every job on the device without any API call; deleting it resumes at once.

`POST /v1/local/reload` makes the agent re-read `config.toml` and apply what it can without a restart: every `[guard]` key, `agent.command_allowlist`, `agent.notify` and `agent.keep_awake`.
It takes a fresh sample at once, so the new rule shows in `LocalStatus.guard` and can start work right away.
The reply is `{"applied": [...], "restart_agent": [...], "restart_coordinator": [...]}`, each a list of `section.name` settings that changed in the file since the agent last read it.
`restart_agent` lists settings the agent reads only when it starts (for example `agent.sample_secs`, `agent.profile`, `agent.listen`, `quota.accounts`); `restart_coordinator` lists `coordinator.*` and `advisor.*`.
A file that does not parse or validate is 400 with the reason, and the agent keeps the settings it has; 503 when the agent loop is gone or does not answer within 12 s.

## Encodings

```json
{"kind": "run", "slots": 2, "mem_mib": 1024}
{"kind": "hold"}
```
`GuardAction`: `run` means running jobs may run and up to `slots` more may start within `mem_mib` of `mem_mb` between them.
`hold` starts nothing and leaves running and paused jobs as they are.
`pause` SIGSTOPs running jobs.
`kill` SIGTERMs them, then SIGKILLs after 15 s.

```json
{"action": {"kind": "pause"}, "resource_action": {"kind": "run", "slots": 2, "mem_mib": 1024},
 "user_active": true, "reasons": ["pause: user active 5s ago (< 600s)"], "at_ms": 1730000000000}
```
`GuardDecision.action` is the combined verdict: use it to start jobs and for jobs with `on_user_active = "pause"`.
`resource_action` ignores user presence: jobs with `on_user_active = "continue"` follow it, and jobs with `on_user_active = "kill"` are killed while `user_active` is true.

```json
{"scope": {"type": "global"}, "action": "pause", "reason": "deploy"}
{"scope": {"type": "device", "id": "my-laptop"}, "action": "resume"}
{"scope": {"type": "account", "id": "claude-max"}, "action": "stop_all"}
{"scope": {"type": "job", "id": 42}, "action": "cancel"}
```
`ControlRequest`; `action` is `pause`, `resume`, `stop_all` or `cancel`.

```json
{"cmd": "pause_job", "id": 7}
{"cmd": "resume_job", "id": 7}
{"cmd": "kill_job", "id": 7}
{"cmd": "stop_all"}
{"cmd": "resume_all"}
```
`AgentCommand` in `HeartbeatReply.commands`.

```json
{"kind": "rolling", "minutes": 300}
{"kind": "weekly"}
```
`QuotaWindowKind`.
`QuotaSnapshot.source` is `claude_statusline`, `codex_sessions` or `manual`.
`Heartbeat.interactive_use` is a list of `[cli, unix_ms]` pairs, e.g. `[["claude", 1730000000000]]`.

## Heartbeat and leases

The agent posts a `Heartbeat` every `agent.heartbeat_secs` (default 30 s).
The reply's `assign` holds newly leased jobs to start now.
The additive `queued_work` boolean reports whether this device has eligible queued work with only the user-idle requirement treated as met.
It uses the same scheduler as assignment, preserving resource budgets, pause controls, device restrictions, command allowlists, retry backoff and quota checks.
An older agent ignores it; an older coordinator omits it, which newer agents treat as `false`.
The opt-in macOS `agent.keep_awake = "ac"` uses this signal while waiting for user idle, or holds for running work, only on known AC power with a clear resource guard.
`Heartbeat`, `LocalStatus`, `DeviceView` and `DeviceExplanation` have an optional `keep_awake` string for the actual assertion state, separate from guard reasons and blockers.
Missing values default to `None` and are omitted when serialized; older agents and coordinators remain compatible.
The coordinator preserves it inside the existing device info JSON, without a schema migration.
`idlemax status` and `idlemax why` print it separately only when present, as of the last heartbeat; `idlemax local status` reports the current local value.
A lease lasts `coordinator.lease_secs` (default 120 s) and is renewed by every heartbeat that reports the job in `running`.
An expired lease re-queues the job with `not_before_ms` backoff; after `max_attempts` it is `parked`.
AI jobs (`cost = "ai"`) get one attempt and never retry automatically.
See [Retries](#retries) for which endings are retried.
A device is offline after `coordinator.offline_after_secs` (default 90 s) without a heartbeat.
A heartbeat is refused with 400 when the device id (128 characters), host name (253) or a capability or AI account entry (128 characters, 64 entries) has a control character or is over its limit.
The coordinator keeps at most 256 devices: a heartbeat from a new one over that is 409, and a device silent for 30 days is forgotten.

### Limits and retention

The store is bounded so one token holder cannot grow it without limit:

- events: messages are cut to 500 characters and only the newest 100000 are kept (and none older than 30 days);
- finished jobs older than 90 days are pruned, except `parked` ones and the last job of a schedule;
- pause flags: at most 1024 (a new one over that is 409; resuming is always allowed), and a pause request for a device id with a control character or over 128 characters is 400;
- advisor inputs: at most 256 KiB per device (400 over that).

### Retries

A job's own result is final.
A non-zero exit, a death by signal or the OS killing it ends the job `failed`.
A kill for its `mem_mb` cap or `max_runtime_secs`, an owner stop (local `stop_all`) and a coordinator `stop_all` end it `killed`, and a cancel ends it `cancelled`.
None of these is retried, whatever `max_attempts` says: running a deterministic failure again would waste the spare capacity idlemax exists to use.

Only a cause that is not the job's fault puts it back in the queue, with a backoff of 2, 4, 8 minutes and so on, capped at 1 hour:

| Cause | Sent as |
|---|---|
| lease lost or expired (agent gone, device offline, coordinator restart) | the sweep re-queues it |
| the guard killed it because the user came back or the machine came under pressure | `killed` with `retryable` |
| the agent was shutting down | `killed` with `retryable` |
| the agent could not start the process | `failed` with `retryable` |
| the agent restarted and reaped the orphan of the previous run | `killed` with `retryable` |

Each of these counts one attempt, and after `max_attempts` (default 3) the job is `parked` for a human.
`attempts` counts leases.
AI jobs are never retried: such a cause parks them at once, and their own failures end `failed`.

### Job updates

The agent posts a `JobUpdate` on every status change of a leased job:

| Status | Sent by | When |
|---|---|---|
| `running` | agent | after a successful start or resume |
| `paused` | agent | after a pause |
| `queued` | agent | lease return: it could not start an assigned job (guard, kill switch, local pause, slots or RAM budget, already running); `error` says why |
| `failed` | agent | the agent could not start the job (`retryable`) |
| `succeeded`, `failed`, `killed` | runner via agent | the process ended, including cap and max-runtime kills |

A `queued` update gives the lease back: the coordinator re-queues the job without counting an attempt.
Terminal updates carry `exit_code`, `output_path` (inbox file on the device) and `output_tail` (last 64 KiB of the attempt's log).
`rate_limited = true` means an AI job hit a usage limit or HTTP 429; that account is blocked for the rest of its window.
`owner_stop = true` marks a `killed` update caused by the owner's local `stop_all`.
`retryable = true` marks an attempt that ended for a reason that is not the job's fault: a guard kill, an agent shutting down, an agent that could not start the job, or an orphan reaped after an agent restart.
Every other `failed` or `killed` update is final, including one from an agent that predates the field.
An update never moves a terminal job out of its terminal status, and an update from a device that does not hold the lease is ignored.
The one exception is a late result: if the agent was away past the lease, the sweep put the job back in the queue (or parked it), and the agent then delivers the terminal update of that attempt, the coordinator applies it as if it had arrived in time.
That needs `attempt`, the lease count of the attempt the update belongs to (`Job.attempts` when it was leased), to equal the stored job's `attempts` while the job has no device.
A job leased again has more attempts or a device, so a stale result never overwrites a newer run, and an old agent sends no `attempt` (0), so its late results stay ignored as before.

Delivery is at least once and application is exactly once.
The agent appends each update to `agent/outbox.jsonl` (fsynced) before it sends it, in order, and drops it once the coordinator acks its `seq`.
After a crash or restart it replays whatever was never acked, so a `SIGKILL` between a job's exit and the ack still delivers that update.
Each update carries `seq`, a per-device number that is unique across restarts (it starts at the wall clock in milliseconds and the journal keeps it rising).
The coordinator stores `(device, seq)` in the same transaction as the job state, its event and any quota effect, and a replay of a stored `seq` changes nothing and is acked again.
The reply is `{"acked_seq": <seq>}`; `seq` 0 (an agent without the outbox) is applied without deduplication and answered without an ack, and an old coordinator's `{}` counts as an ack of the update just sent.
A stale non-terminal update after a terminal one never reopens the job.
The advisor's AI pass ingests its output after storing the result and acks the update only afterwards.
If the coordinator dies in between, the replayed update finds the job already succeeded but no Advice event naming it, and ingests the output again.
The journal holds at most 1000 updates; when it is full the oldest non-terminal update goes first, so exit codes survive a long outage, and the file is compacted when it is about twice that size.
A torn last line from a crash is skipped.
The coordinator answers 4xx (other than 401, 403, 408 and 429) only when an update can never succeed, such as an unknown job id or a job leased to another device; the agent then drops it.
Auth failures, timeouts and 5xx keep the update queued for retry.
The agent sends heartbeats from their own task, so a slow coordinator delays heartbeats but never local controls or guard actions.

## Why a job is not running

`GET /v1/jobs/{id}/why` returns an `Explanation` (`idlemax why <id>` prints it, `--json` prints this body).
It is computed from the coordinator's current state with the scheduler's own checks, so it cannot disagree with what the scheduler does.
The device-side verdicts (guard, running jobs, slot and RAM budget) are as of that device's last heartbeat, shown in `devices[].last_seen_ms`.

```json
{"job": 7, "name": "nightly-make", "status": "queued", "state": "blocked",
 "summary": "Job 7 (nightly-make) is queued but not starting.",
 "reasons": [{"kind": "backoff", "message": "waiting out the retry delay: attempt 2 of 3 starts in 1m 10s (last error: lease expired on my-laptop)",
              "fix": null, "until_ms": 1730000070000}],
 "devices": [{"device": "my-laptop", "online": true, "last_seen_ms": 1730000000000, "ready": false,
              "ineligible": [],
              "reasons": [{"kind": "guard", "message": "on battery (guard pause)",
                           "fix": "plug in, or set guard.require_ac_power = false", "until_ms": null}]}],
 "at_ms": 1730000001000}
```

- `state`: `ready` (a device leases it on its next heartbeat), `blocked` (queued or parked, with reasons), `active` (leased, running or paused) or `finished`.
- `reasons` hold on every device (pauses, retry backoff, AI quota, a busy AI account, no online device).
  `devices[].reasons` hold for that device only; `ready` is true when the device has none and neither does the job.
- `devices[].ineligible` lists, as short phrases, why a device can never take the job until the job spec or the device's config changes: `limited to mac` (the job's `devices` list), `no cargo` (a `requires` tool), `does not take cpu_heavy jobs` (the profile's cost classes), `ls is not on its allowlist`, `may not spend claude-max`.
  It is empty for a device that only has to wait, and absent from a coordinator older than this field, so treat a missing list as eligible.
- `kind` is a stable snake_case code for scripts: `global_pause`, `account_pause`, `job_pause`, `device_pause`, `local_pause`, `backoff`, `parked`, `no_device_online`, `offline`, `device_filter`, `missing_capability`, `cost_class`, `command_not_allowed`, `ai_no_account`, `ai_account_not_on_device`, `ai_quota`, `ai_account_busy`, `guard`, `no_slot`, `ram_budget`, `queue_order`.
  `message` is plain language, `fix` is the command or config key that clears it (null when it clears by itself), `until_ms` is set for `backoff`.
- A `finished` job has no reasons, and a `parked` job has one `parked` reason carrying its last error.

`GET /v1/why?limit=` is the cheap form for a list of jobs, which the web dashboard polls: one `WhySummary` per queued, leased or paused job, newest first (default 100, at most 1000), computed from one read of the coordinator's state.
Finished and parked jobs are left out; a waiting job nothing holds back is listed with a null headline.

```json
{"job": 7, "status": "queued", "state": "blocked", "more": 2,
 "headline": {"device": "my-laptop",
              "reason": {"kind": "guard", "message": "on battery (guard pause)",
                         "fix": "plug in, or set guard.require_ac_power = false", "until_ms": null}}}
```

- `headline` is the reason worth showing beside the job, `null` when nothing holds it (it is ready, running or finished).
  A reason that holds on every device comes first, with `device` null.
  Otherwise it is the first reason of the device closest to running: one that only waits and needs no setting changed, then an online one, then the one with the fewest reasons.
- `more` counts the other reasons in the full explanation that belong with the headline: those on every device plus those of the headline's device.
  Fetch `GET /v1/jobs/{id}/why` for the whole list when a user asks for it.

`DeviceInfo.command_allowlist` is the agent's `agent.command_allowlist`, sent with every heartbeat.
The scheduler leases a job only to a device whose list contains the job's `argv[0]`, because the agent would refuse it otherwise.
It is `null` for an agent that predates the field, and such a device is not filtered.

## Control semantics (coordinator)

- Commands are level-triggered: every heartbeat reply re-derives `pause_job`, `resume_job` and `kill_job` from coordinator state, so a lost reply heals itself.
  The coordinator never sends `stop_all` or `resume_all`; those exist for the agent's local control.
- `pause` adds the scope to the paused set: nothing new is leased in it, and its active jobs get `pause_job`.
- `resume` removes exactly that scope; resuming `global` does not clear device, account or job pauses.
  `resume_job` goes to reported paused jobs that no coordinator pause still holds.
- `stop_all` pauses the scope and marks its active jobs `killed` (`last_error = "stop_all: <reason>"`).
  A reported job whose stored status is terminal, or that is leased to another device, gets `kill_job`.
- `pause` and `stop_all` on a `device` that has never sent a heartbeat, or an `account` not in `[[quota.accounts]]`, are 400 and the error lists the known names, so a typo never reports success.
  `resume` is never refused this way, so a flag under any name can always be removed.
- `cancel` needs `job` scope (400 otherwise).
  A queued or active job becomes `cancelled` (active ones get `kill_job`); a finished job is 409.

## Advice

Each agent posts `AdvisorInputs` to `/v1/agent/advisor-inputs` every `agent.advisor_inputs_secs` (default 900 s, 0 sends nothing): `{device, github, sessions}`.
`github` is the read-only `gh` summary (review requests, assigned PRs and issues), `sessions` is metadata of recent Claude and Codex sessions (tool, id, title, project, last activity, signals), never message text.
All fields default, so an agent with fewer or more fields still decodes.
The coordinator keeps the newest copy per device and the advisor uses the newest good GitHub summary plus the sessions of every device whose copy is under 3 days old.
A coordinator older than this route answers 404; the agent logs a warning and tries again next interval.

`Recommendation.key` is a stable dedup key (for example `parked_job:42`), so a refresh does not repeat advice the owner already saw.
Accepting queues `suggested_job`; nothing runs on the advisor's word alone, except job names listed in `advisor.auto_accept`.
Suggested jobs come from templates in `$IDLEMAX_HOME/jobs/*.toml`.
Refreshes and AI-output ingestion are serialized; each plan commits advice, queued jobs, cancellations and events atomically.
A failed job admission leaves the plan unapplied, and a stale refresh can only dismiss advice that is still open.
AI-pass jobs use the per-account idempotency key `advisor-ai-pass:<account>` so an unfinished pass is not queued twice.

## Schedules

A schedule repeats a job.
Types live in `crates/idlemax-core/src/schedule/`.

```json
{"spec": {"name": "repo-health", "command": ["/path/to/repo-health.sh"], "cost": "cpu_heavy"}, "every": "1d"}
```
`NewSchedule`, the body of `POST /v1/schedules`.
`spec` is a `JobSpec`.
`every` is a whole number and one unit: `s`, `m`, `h` or `d`, for example `90m`, `6h` or `1d`.
The minimum is `1m` and the maximum is `365d`.
Responses write the interval in the largest unit that divides it evenly, so `24h` reads back as `1d`.

```json
{"id": 1, "spec": {"name": "repo-health", "...": "..."}, "every": "1d", "paused": false,
 "created_ms": 1730000000000, "last_fired_ms": 1730000100000, "last_job": 41,
 "decision": {"kind": "running", "job": 41, "status": "queued"}}
```
`ScheduleView` is the schedule with its current `decision` and, once the last queued job has ended, `last_result`: `{"status": "failed", "exit_code": 2, "error": "...", "finished_ms": N}`.
`last_result` is absent while that job is still active or before the first run.
The `decision` is one of:

| `decision` | Meaning |
|---|---|
| `{"kind": "fire"}` | Due; the next sweep queues a job |
| `{"kind": "wait", "until_ms": N}` | The interval has not passed; the next job is due at `N` |
| `{"kind": "running", "job": N, "status": S}` | The last job is still `queued`, `leased`, `running` or `paused` |
| `{"kind": "paused"}` | The schedule is paused |

Semantics:

- The coordinator checks every 15 s.
  It queues one job when the schedule's previous job has ended and a whole interval has passed since it ended (since it was queued, if it never ran).
  A job that is still queued, leased, running or paused blocks the next one, however long it takes.
- The queued job is an ordinary job, so the guard and scheduler still decide when and where it runs.
  "Every day" therefore means once a day, whenever the machine is idle.
- After downtime or a long wait for an idle machine, one job is queued, never one per missed period.
- A new schedule queues its first job at once, so the owner sees it work.
- Failed, killed, cancelled and parked jobs do not stop the schedule; the next job follows after the interval.
- `schedule_fired` events (kind `schedule_fired`) record each queued job.
  Adding, pausing, resuming and removing a schedule log `control` events.
  If a job cannot be queued (for example its `ai_account` was removed from `[[quota.accounts]]`), an `error` event says why and the schedule tries again after the next interval.
- `POST /v1/schedules` answers 201.
  It answers 400 for a spec that fails `JobSpec::validate` or names an `ai_account` missing from `[[quota.accounts]]`, and 422 for a body that does not decode (including a bad `every`).
  It answers 409 when a schedule with the same job name exists, so the same job cannot be added twice.
- `DELETE /v1/schedules/{id}` leaves a job the schedule already queued alone; the returned `decision` names it.
  Cancel it with `POST /v1/jobs/{id}/cancel`.
- Pause, resume and delete answer 404 for an unknown id.
- Operator responses never include `spec.stdin`.
- A client older than this release cannot decode the `schedule_fired` event kind from a newer coordinator, so upgrade the CLI and TUI together with the coordinator.
