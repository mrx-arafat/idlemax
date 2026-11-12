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
| POST | /v1/agent/dispatch-inputs | `DispatchInputs` | `DispatchReply` |
| GET | /v1/status | | `ClusterStatus` |
| GET | /v1/jobs?status=&limit= | | `[Job]` |
| GET | /v1/jobs/{id} | | `Job` |
| POST | /v1/jobs | `JobSpec` | `Job` |
| POST | /v1/jobs/{id}/cancel | | `Job` |
| GET | /v1/jobs/{id}/why | | `Explanation` |
| GET | /v1/why?limit= | | `[WhySummary]` |
| GET | /v1/quota | | `[quota::QuotaView]`; a row carries `limited` while a usage limit seen in the current window rests the account |
| GET | /v1/report?days=&since_ms=&label= | | `Report` |
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
| POST | /v1/dispatcher/mode | `{"mode": "off" \| "propose"}` | `{"mode", "before", "changed"}` |
| GET | /v1/plan | | `Plan` |
| POST | /v1/plan/refresh | | `Plan`, after a dispatcher step that asks for an orchestrator pass |
| GET | /v1/projects?device= | | `[ProjectView]`, by device then name |
| POST | /v1/projects | `NewProject` | `ProjectView`, 201 when new and 200 when known |
| POST | /v1/projects/{id}/on | | `ProjectView` |
| POST | /v1/projects/{id}/off | | `ProjectView` |
| POST | /v1/projects/{id}/settings | `ProjectSettings` | `ProjectView` |
| GET | /v1/runs?state=&project=&limit= | | `[Run]`, newest first |
| GET | /v1/runs/{id} | | `Run` |

`status` is a `JobStatus` (`queued`, `leased`, `running`, `paused`, `succeeded`, `failed`, `killed`, `cancelled`, `parked`).
`POST /v1/jobs` returns 201 with the queued job; it answers 400 for a spec that fails `JobSpec::validate`, names an `ai_account` missing from `[[quota.accounts]]` or carries a `worker` section (`worker is set by the dispatcher only`), and 409 when 10000 jobs are already unfinished.
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
| `worker` | texts 4096 bytes, `done_check` 128 KiB, lists 64 entries of up to 4096 bytes, no NUL byte |
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
The guard verdict in `Heartbeat`, `LocalStatus` and `DeviceView` may carry optional `memory_limit` metadata: `{configured_mib, effective_mib, cgroup_max_bytes}` when Linux clamps RAM to its effective v1/v2 ancestor cap minus 128 MiB of agent overhead.
When `cgroup_max_bytes` is absent inside this metadata, the Linux server could not determine its limit and uses a reference fallback of at most 1 job/512 MiB, preserving stricter overrides; this differs from a successfully read unlimited hierarchy, which has no fallback metadata.
`DeviceExplanation.memory_limit` carries the same metadata; missing fields default to absent and absent fields are omitted, so old agents and coordinators remain compatible.
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

`JobSpec.admission` is `guard` (the default, and absent on the wire) or `lines`.
Only the coordinator sets `lines`, on the dispatcher's own jobs (its orchestrator pass, a project's setup and check commands): `POST /v1/jobs`, schedules and advice accept refuse it with 400, and `idlemax job add` refuses it in a spec file.
A dispatcher job is admitted by the device's worker room (`devices[].room`) instead of the guard: it leases and starts while the room is `run` with an open slot, whatever user idle time or the memory pressure level say, holds on `hold`, and is killed (retryable) on `shed` or `emergency`.
At most one dispatcher job is leased or running per device, it uses none of the guard's slots or RAM budget, and it skips only the quota-headroom check, because the dispatcher admits its account when it queues the job.
A pause, the kill switch, `stop_all`, the command allowlist, retry backoff and the one-AI-lease rule apply to it as to any job.
An agent that predates the field treats it as a plain job.

`JobSpec.worker` turns a job into one step of a worker Run, and only the coordinator's dispatcher sets it: `POST /v1/jobs`, schedules and advice accept refuse it with 400, and `idlemax job add` refuses it in a spec file.
It is absent on the wire for a plain job, and a spec with it must have `cost = "ai"`.
It holds `run` (the Run id), `action` (`launch`, `resume`, `keep`, `keep_merge` or `discard`), `project`, `task` (the task slug), `task_title`, `done_check` (text for the worker and reviewer, never run by idlemax), `repo`, `base_branch`, `branch` and `worktree` (set for every action except a first `launch`), `cli` (`claude`, `codex`, `opencode` or `agy`), `model`, `session`, `setup` and `check` (argv lists), `baseline` (`{"base_commit", "passed"}` from the dispatcher's check job, when known), `scope_paths`, `off_limits`, `core_paths`, `reviewer` (`{"cli", "model", "account"}`), `review_diff_lines`, `blocked_secs`, `setup_secs` and `check_secs`.
The brief travels in `stdin`, which no operator response returns.

### Limits and retention

The store is bounded so one token holder cannot grow it without limit:

- events: messages are cut to 500 characters and only the newest 100000 are kept (and none older than 30 days);
- finished jobs older than 90 days are pruned, except `parked` ones and the last job of a schedule;
- guard spans (see [What idlemax did](#what-idlemax-did)): at most 10000 per device (the oldest go first, so a device that flaps only loses its own history), none older than 30 days; and pause spans: at most 10000, closed ones going first, none closed more than 30 days ago;
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

A worker job (one with a `worker` section) is never queued again and never parked; its Run decides whether the work is launched again.
A returned lease ends it `cancelled`, a `retryable` end ends it `killed`, and a lease that ran out ends it `killed`, and in each case its Run becomes `shed` (back in the queue).
When the Run was still `starting` for the third launch in a row, it becomes `needs_you` with the concern `could not be started 3 times: <last error>`.
An owner stop (local `stop_all` with `owner_stop`, a coordinator `stop_all` or `cancel`) makes the Run `stopped`, which only the owner's start moves on.
Any other `failed` or `killed` end makes it `needs_you` with the job's error as the concern, and a `succeeded` launch leaves it in `checking` for its verdict.
After the coordinator itself slept (a gap of more than two sweep periods, 30 s), the first sweep renews a worker lease that ran out instead of ending it, since the agent on the same machine slept too.

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
`worker` is present only on the updates of a worker job, and holds `run`, `phase` (`preparing`, `running`, `checking` or `done`), `session`, `branch` and `worktree`.
The last update of a launch carries them again, plus `facts` (what the device found in the worktree after the turn) and `end_text`: the text that ended a turn in which no commit was added, at most 2,000 characters, and the only output of a worker that leaves the device.
The Run of a worker job moves in the same transaction as the job: phase `running` makes it `running`, phase `checking` makes it `checking`, the session, branch and worktree are stored when sent, and `end_text` is kept in the Run's `facts`.
An update that the states do not allow, such as one for a Run the owner stopped, leaves the Run as it is, and a replayed update changes nothing.
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

## How much of each device is used

`GET /v1/status` (and the reply of `POST /v1/control`) answers "how much is used, how much is idle, and why" without a `why` call per job.
Each entry of `devices` carries `capacity`, and the cluster carries `accounts`.
`idlemax status`, the TUI Devices tab and the dashboard device cards show these fields; none of them works the answer out again.
It is computed from the same snapshot and the same scheduler checks as `why`, so the two cannot disagree.

```json
{"devices": [{"info": {"id": "my-laptop"}, "online": true,
              "capacity": {"state": "cannot_run", "slots_used": 1, "slots_allowed": 2,
                           "mem_used_mib": 256, "mem_allowed_mib": 1024,
                           "why": "1 queued job cannot run here: no docker"}}],
 "accounts": [{"account": "claude-max", "spendable": true, "paused": false, "headroom_pct": 31.0,
               "window": {"kind": "weekly"}, "resets_at_ms": 1730500000000, "why": null}]}
```

- `capacity.slots_used` and `mem_used_mib` count every job that holds a slot: running, paused, and leased but not yet started.
  `mem_used_mib` is the sum of their `mem_mb`.
- `slots_allowed` and `mem_allowed_mib` are the most the device may hold: used plus what the guard's last verdict says is free.
  They are `null` while the guard, a pause or the PAUSE file keeps new jobs from starting, because only a verdict that lets jobs start says how many more fit.
- `state` is a stable snake_case code: `offline`, `held` (the guard or a pause keeps new jobs from starting), `full` (no free slot or no free RAM budget: the device works as hard as it may), `starting` (room and ready work: it leases on its next heartbeat), `too_big` (the smallest ready job needs more RAM than is free), `cannot_run` (no queued job can run on this device as it is configured), `waiting` (queued jobs wait on a retry delay, AI quota, a busy AI account or a pause), `idle` (room and nothing queued).
- `why` is plain language for the state: `held: memory pressure` uses the same labels as `idlemax report`, `cannot_run` names the cause most queued jobs share (`no docker`, `limited to server`, the same phrases as `devices[].ineligible` in a `why` reply), and `full` says how many ready jobs wait.
  A device that is `full` or `starting` is doing what it can; `idle`, `cannot_run`, `too_big` and `waiting` are spare capacity with a reason.
- `accounts` has one entry per configured AI account, in the order of `[[quota.accounts]]`, and is `[]` when none is configured.
  `spendable` and `why` say whether idlemax may spend the account this minute and, if not, the first reason (an interactive session, a lease, a window used up, the timing rule).
  `headroom_pct` is a separate question: the percent of quota left to spend above the reserve, in the window that limits the account, whether or not it may be spent now, so a shut account can still show plenty left.

Each entry of `devices` also carries `room`, the worker room of the agent's newest sample: the line state of the dispatcher (`run`, `hold`, `shed` or `emergency`), the reading that set it, and how many workers fit under the hold line.
The agent sends it in every heartbeat as `Heartbeat.room`, and `GET /v1/local/status` on the agent shows the same value.

```json
{"room": {"state": "hold", "reason": "hold: memory 91% used (hold at 90%)", "open_slots": 0, "at_ms": 1730000000000}}
```

The room reads memory used percent, the swap-in rate, free disk, the battery floor and thermal throttling, and never user idle time or the memory pressure level, so it differs from `guard`.
`open_slots` is 0 unless the state is `run`.
On Linux, where memory used percent is not read yet, the room is `hold`.
An agent that predates the field sends no `room`, and the entry has none.
  It is `null` when that window has no trusted data (no snapshot, stale, or waiting for the first one after a reset).
  The limiting window (`window`, `resets_at_ms`) is the one that blocks the account: a window with no data first, else the blocking window that resets last; with none blocking, the window with the least headroom.
  `GET /v1/quota` still has every window in full.
- Both fields are additive.
  A coordinator that predates them omits `capacity` and `accounts`, and a client must treat that as "unknown", not as empty: `idlemax status` then shows the running count for a device and no accounts list.

## What idlemax did

`GET /v1/report` answers "did it help, and what stopped it?" for one period ending now (`idlemax report` prints it, `--json` prints this body).
Give `days` (1 to 30) or `since_ms` (Unix ms, within the last 30 days), not both; neither means the last 24 hours.
A `since_ms` at or after the coordinator's now is read as now, so a client whose clock runs a little ahead gets an empty period instead of an error.
`label` names the period in `verdict` (1 to 32 letters, digits, spaces, `.`, `_`, `-`); it defaults to `last 24 h`, `last N days` or `this period`.
A bad parameter is 400.

```json
{"label": "today", "since_ms": 1730000000000, "until_ms": 1730036000000,
 "verdict": "today: 2 jobs ran (14 job-min); guard held 6.1 h, mostly memory pressure (5.2 h); queue was empty 1.3 h of idle time",
 "jobs": {"ran": 2, "succeeded": 2, "failed": 0, "killed": 0, "cancelled": 0, "job_ms": 840000},
 "guard": {"tracked_ms": 36000000, "tracking_since_ms": 1729990000000, "held_ms": 21960000,
           "held_by": [{"reason": "memory", "label": "memory pressure", "ms": 18720000}],
           "offline_ms": 0},
 "idle": {"used_ms": 840000, "queue_empty_ms": 4680000, "work_waiting_ms": 0,
          "quota_waiting_ms": 0, "stuck_jobs": [],
          "longest": {"start_ms": 1730001000000, "end_ms": 1730020000000,
                      "cause": {"kind": "held", "reason": "memory"}, "label": "held by memory pressure"}},
 "quota": {"configured": false, "counted_since_ms": 1730000000000, "expired": [], "open": []}}
```

- Times are device time: with two devices, 24 hours of period is 48 hours of `tracked_ms`.
  `tracked_ms` counts from the first guard sample the coordinator kept (`tracking_since_ms`), so a period that starts before it is covered only from then.
- `jobs` counts jobs that ran at some point in the period (`ran`), and those that ended in it by how they ended.
  `job_ms` is wall-clock time from start to finish, paused time included, summed over every attempt.
  A job row keeps only its last attempt, so the attempts come from the job's start and end events (`job_started`, then `job_succeeded`, `job_failed`, `job_killed`, `job_cancelled`, `job_parked`, `job_queued` or `lease_expired`), which carry the coordinator's clock; a job with no such events falls back to its row.
  Every time is clamped to between the job's creation and now, because the start and finish in a row are stamped by the agent's clock, which can run ahead or behind.
  The jobs that ended in the period, and how, are counted on the coordinator's clock too: from the time it logged `job_succeeded`, `job_failed`, `job_killed` or `job_cancelled`, falling back to the row's `finished_ms` when those events are gone.
- `guard.held_ms` is time the guard held new work (verdict hold, pause or kill).
  `held_by` splits it by reason, longest first; one verdict can have several reasons, so the parts can add up to more than `held_ms`.
  `reason` is one of `pause_file`, `local_pause`, `paused` (a coordinator pause from `idlemax pause`: all jobs, or this device), `user_active`, `idle_unknown` (the host cannot read the idle time, so the guard holds), `battery`, `thermal`, `memory` (memory and swap), `disk`, `load` (CPU load and I/O pressure), `recovering` and `other`.
- `guard.offline_ms` is time with no heartbeat for longer than `coordinator.offline_after_secs`: the machine slept, or the agent or coordinator was down.
  A device whose newest guard span ended more than 7 days before the period began is taken to be retired and is left out of it, and the zero-job verdict names "asleep or offline" only when no device was awake.
  A pause on one job, on an AI account or on all jobs is not waiting: the scheduler's own `in_scope` takes that job's time out of its wait, and a pause on all jobs or on a device holds the device (`may_assign`), which counts under `paused` above.
- `idle` splits the awake time that no guard hold covers: `used_ms` (a job ran), `queue_empty_ms` (nothing the device could take was waiting), `work_waiting_ms` and `quota_waiting_ms`.
  `work_waiting_ms` is time a device was idle with a queued job waiting past two minutes (a heartbeat to learn of it, another to lease it) that it could take yet did not start: `idlemax why <id>` says why.
  A device could take a job when the scheduler's own `device_blockers` finds nothing against it (the job's `devices` list, `requires`, cost class, the device's command allowlist and AI accounts), so a job pinned to an offline server is not waiting on an idle laptop.
  `work_waiting_ms` and `quota_waiting_ms` are wall-clock time counted once however many devices could take the work, while the other numbers are device time.
  An AI job waits for quota or its account, not for room, so it counts in `quota_waiting_ms`.
  A retry's backoff is not counted as waiting.
  `stuck_jobs` lists queued jobs (at most 5, lowest id first) that have waited past that grace and that no known device could take as it is configured, such as a command missing from every allowlist.
  `longest` is the longest unbroken stretch with nothing running while the device was awake, and `cause` is what it mostly was: `{"kind": "held", "reason": ...}`, `{"kind": "work_waiting"}`, `{"kind": "waiting_on_quota"}` or `{"kind": "queue_empty"}`.
- `quota` is filled only when `[[quota.accounts]]` is configured (`configured` is false and both lists are empty otherwise).
  `expired` lists window cycles whose reset fell in the period with the most percent seen used (`peak_used_pct`) and what was left (`unused_pct`); `open` lists the cycles still running.
  The coordinator keeps quota snapshots for `coordinator.metrics_retention_days` (7 by default), so a longer period counts cycles only from `counted_since_ms`, which is the period's start or the start of the kept history, whichever is later; the verdict and `idlemax report` say so.
  Use seen between the last probe and the reset is not known, so `unused_pct` is an upper bound.

The coordinator keeps one guard span per device per verdict change (`guard_spans`, schema V7): a heartbeat that repeats the verdict only moves the span's end, so the table grows with changes, not samples, even if the coordinator's clock steps back.
A report that starts while a device is asleep still counts the sleep, because the query also takes the device's newest span from before the start (one indexed lookup per device, since the query runs under the store lock that every heartbeat waits for).
Pauses are kept the same way in `pause_spans`: a pause opens a span, a resume closes it, and a pause already in force when V7 first runs starts then.
Spans older than 30 days are pruned, and a coordinator upgraded to V7 has no history from before the upgrade.
A device older than this field needs no change: the coordinator reads the verdict from the heartbeats it already sends.

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

## The dispatcher plan

`GET /v1/plan` answers what the dispatcher would launch now and why; it launches nothing.
Types live in `crates/idlemax-core/src/dispatch/plan.rs`.

```json
{"at_ms": 1730000000000, "mode": "propose", "picked_by": "no orchestrator pass yet",
 "devices": [{"device": "mac",
              "room": {"state": "run", "reason": "run: memory 70% used (hold at 90%), swap-in 0 pages/s, disk 9000 MiB free", "open_slots": 2, "at_ms": 1730000000000},
              "held": null, "picks": [],
              "empty": {"count": 2, "reason": "1 ready tasks are not judged by the orchestrator: no orchestrator pass yet"}}],
 "not_judged": [{"id": "roadmap:3:faster-sync", "project": "app", "project_id": 3, "device": "mac", "source": "roadmap",
                 "title": "Faster sync", "done_check": "sync takes under 1 s", "scope_paths": [], "needs_you": null}],
 "needs_you": [],
 "accounts": [{"account": "claude-max", "provider": "claude", "usable": true, "why": "usable: 5-hour window 20% used, weekly 40% used",
               "five_hour_used_pct": 20.0, "weekly_used_pct": 40.0}],
 "notes": ["app: the check has not run on 4f2a9c1e0b7d"]}
```

- `mode` is `off` or `propose`; in `off` no device takes a pick.
- `picked_by` names the orchestrator pass the picks come from and its age, or why no pass has run.
- Per device, `room` is the worker room the agent reported (`hold` with `this agent reports no worker room` when it sent none), and `held` says why the device takes nothing whatever its room: `offline`, `paused by you: idlemax resume`, or `kill switch or local pause is on`.
  A device takes picks only in `propose` mode, not held, with the room at `run`; it has `room.open_slots` slots.
- Each pick names `project`, `task`, `title`, `provider`, `account`, `model`, the orchestrator's `reason`, `by: "orchestrator"`, the `scope_paths` the orchestrator named and the `avoid_paths` a worker must not touch.
  A pick comes only from an orchestrator pass and only when its task is offered to that device, its account is usable and listed in the device's `agent.ai_accounts` with the provider's CLI installed, and its model is in `dispatcher.worker_models`.
  A task in a project the owner is active in is picked only when no other project of the device has a ready task, never while the owner's changed-file list is cut, and then carries those files in `avoid_paths`.
- `empty` counts the slots left over with one reason: no ready task, tasks the orchestrator has not judged, why the next pick could not be used, or that the orchestrator picked nothing more.
- `not_judged` lists the ready tasks no orchestrator pass has seen, in plan order; they never fill a slot.
- `needs_you` lists the tasks that need the owner, each with its reason in `needs_you`.
- `accounts` applies the simple rule: an account is usable while its 5-hour window is under 60 percent used and idlemax has spent under 10 percent of its weekly quota, with a fresh weekly reading and no usage limit in the current window; agy and API accounts have no reading and are not usable.
- `notes` holds per-project lines, such as a focused check that has not run on the base commit yet and why, and `<device>: herdr gave no folders` for a device whose newest feed came without a Herdr view.

### The orchestrator pass

The coordinator's dispatcher step (every minute, and on `POST /v1/plan/refresh`) queues one `dispatch-orchestrator-pass` job when a pass is due: the mode is `propose`, a device takes picks and has a ready task, a usable Claude account is listed on such a device with `claude` installed, no pass job is active, and, unless the pass was asked for, its inputs changed since the newest pass and that pass was queued at least an hour ago.
The inputs are the devices that take picks, every ready task and every task a stored judgment holds (by id and a digest of source, title and done check), the usable accounts, the models and the text of `rules.md`.
A queued pass that stops being due for any other reason is cancelled with that reason.

The job runs `claude -p --model opus --settings {"disableAllHooks":true} --strict-mcp-config --output-format json --json-schema <schema> --no-session-persistence --tools ""` with the prompt on stdin, on the chosen account and device, admitted by the worker lines.
Its answer must be:

```json
{"picks": [{"task": "roadmap:3:faster-sync", "account": "claude-max", "model": "sonnet",
            "reason": "small, and its done check is a command", "scope_paths": ["src/sync.rs"]}],
 "needs_you": [{"task": "roadmap:3:dark-mode", "reason": "the item needs a design decision"}]}
```

At most 8 picks, 20 needs-you entries, 300 characters per reason and 20 scope paths of at most 300 characters per pick; a shape error, an unknown field or a length over a limit rejects the whole answer.
A pick whose task, account or model was not offered, a scope path that is absolute or holds `..`, a task picked twice and a task also under `needs_you` (needs-you wins) are dropped one by one and counted in `picked_by`.
Each `needs_you` task is stored as a judgment that lasts until the task's digest changes.
The plan uses the newest pass with a result; `picked_by` reads `orchestrator pass <age> ago`.

## Dispatcher mode

`POST /v1/dispatcher/mode` writes `dispatcher.mode` into the coordinator's `config.toml` with the same editor `idlemax config set` uses: every other line and comment stays, and the old file is kept as `config.toml.idlemax.bak`.
`off` plans nothing; `propose` shows what the dispatcher would launch and launches nothing.
Any other value is 400 and the file is left as it was.
The reply is `{"mode": "off", "before": "propose", "changed": true}`; `changed` false means the file already said so and nothing was written.
A change appends one `control` event, such as `dispatcher mode propose -> off`.
Both daemons read the `[dispatcher]` section again when the file's mtime changes, so the switch holds with no restart.
The route is an owner action: no pause, kill switch, guard or line state holds it.

## Projects

A project is a git repository on a device that the dispatcher may work in.
Every project is off until the owner switches it on, and idlemax writes nothing into the repository.
Types live in `crates/idlemax-core/src/dispatch/project.rs`.

```json
{"id": 1, "name": "app", "device": "mac", "path": "/Users/me/app", "enabled": true,
 "base_branch": "main", "setup_command": [], "check_command": ["cargo", "test", "-p", "core"],
 "off_limits": [], "core_paths": ["crates/core"], "priority": 0,
 "source": "owner", "owner_active": false, "seen_ms": null}
```
`ProjectView`: the project, its settings flattened into it, `source` (`herdr` or `owner`), `owner_active`, `seen_ms` and, after a settings change, `warning` when the device's `agent.command_allowlist` would refuse the setup or check command.

- `NewProject` is `{"path": "/abs/path", "device": "mac", "name": "app"}`; `device` defaults to the only known device (400 naming the known ones otherwise) and `name` to the folder name.
  A known `(device, path)` answers 200 with the existing row; a taken name gets `-2`, `-3` and so on.
  A device holds at most 200 projects (409 beyond).
- `ProjectSettings` replaces all six settings at once.
  The commands are argv, never a shell line.
  400 for a base branch that is empty, starts with `-` or holds whitespace, an empty first command word, or an off-limits or core path that is absolute or holds `..`.
- An unknown id is 404.
- Each add, switch and settings change appends one `control` event.
- These routes are owner actions: a pause, the kill switch, the guard or the line state never holds them.

## Runs

A Run is the record of one dispatcher worker on one task, from its first launch to the owner's review.
Types live in `crates/idlemax-core/src/worker/run.rs`.

```json
{"id": 4, "device": "mac", "project": "app", "task": "roadmap:3", "task_title": "3. Faster sync",
 "source": "roadmap", "branch": "idlemax/roadmap-3-1a2b3c", "worktree": "/Users/me/.idlemax/worktrees/app/roadmap-3-1a2b3c",
 "provider": "claude", "account": "max", "model": "sonnet", "session": "0b6e...",
 "state": "running", "verdict": null, "concern": null, "reason": "the oldest ready roadmap item",
 "summary": null, "facts": {}, "job": 17, "launches": 1, "start_tries": 0,
 "start_ms": 1762900000000, "end_ms": null, "updated_ms": 1762900060000,
 "quota_before": [QuotaSnapshot], "quota_after": []}
```

- `state` is one of ten values, and only these moves happen:
  `starting` to `running`, `stopped`, `shed` or `needs_you`;
  `running` to `checking`, `stopped`, `shed` or `needs_you`;
  `checking` to `clean`, `needs_you`, `rejected`, `stopped` or `shed`;
  `stopped` (the owner's stop) to `shed` (the owner's start) or `discarded`;
  `shed` (back in the queue) to `starting` or `discarded`;
  `clean`, `needs_you` and `rejected` to `kept` or `discarded`;
  `kept` and `discarded` to nothing.
- `verdict` is `clean`, `needs_you` or `rejected`, and stays after a keep or a discard; `concern` says why for the last two.
- `reason` is why the task was picked, `source` is the task's source, `job` is the live or last job of the Run, `launches` counts its jobs and `start_tries` the launches in a row that never reached `running`.
- `quota_before` and `quota_after` hold the account's readings per window at the first launch and at the end of the last one.
- `GET /v1/runs` filters by `state` (400 for an unknown one) and `project`, and answers with at most `limit` runs (default 100, at most 1000).
- `GET /v1/runs/{id}` answers 404 with `run <id> not found` for an unknown id.
- The brief and the worker section of the first launch are stored with the Run for its relaunches, and no route returns them.
- A kept or discarded Run is pruned 90 days after it ended.

A worker job is an AI job (`cost = "ai"`) that stands outside the one-AI-lease rule and the one-start-per-5-hours rule in both directions: several worker jobs may hold one account, a worker job never blocks a plain AI job, and a plain one never blocks a worker job.

## Dispatch inputs

Every `agent.dispatch_inputs_secs` (default 60, 0 sends nothing) an agent sends what it knows about the git repositories on its device.
Unlike the advisor inputs, which carry only folder names, these carry full project paths.
Types live in `crates/idlemax-core/src/dispatch/inputs.rs`.

```json
{"device": "mac", "herdr": true,
 "repos": [{"path": "/Users/me/app", "name": "app", "slug": "me/app", "default_branch": "main",
            "working": true, "base_commit": "4f2a...", "touched": ["src/lib.rs"], "touched_clipped": false,
            "roadmap": [{"title": "3. Faster sync", "done_when": "sync takes under 1 s", "done": false, "owner": false}],
            "checkout": null}]}
```

- The repositories are the folders of the owner's Herdr panes and agents, plus every switched-on project of the last reply, also when Herdr is missing (`herdr: false`).
  A folder inside `$IDLEMAX_HOME` is never the owner's and is skipped.
- `working` is true when a Herdr agent with status `working` runs in the repository.
- `default_branch` is the local branch `origin/HEAD` names, kept only when it exists locally.
- `base_commit`, `roadmap` and `touched` are filled only for the switched-on projects of the last reply; `roadmap` comes from `ROADMAP.md`, then `docs/ROADMAP.md`, on the base branch, or at `HEAD` while neither the project nor `origin/HEAD` names one (its items then carry the reason "set the base branch").
- `DispatchInputs::check` refuses more than 64 repositories, 50 roadmap items or 200 touched paths per repository, over-long text and any control character with 400; the store refuses a payload over 256 KiB.
- An unknown repository becomes a project that is off, with `source: "herdr"` and its default branch as the base; a known one is marked seen, and owner-active when `working`.
  A device holds at most 200 projects; a repository beyond that is skipped, never refused.
  A project from Herdr that is off and unseen for 30 days is pruned.
- The reply `{"projects": [Project]}` lists the device's switched-on projects; it is empty while `dispatcher.mode` is `off` or the device is paused.
- `ProjectView.owner_active` is true when an owner session worked in the project within the last 30 minutes, from this feed or from the advisor's session metadata.

