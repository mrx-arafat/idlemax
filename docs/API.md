# idlemax HTTP API

Types live in `crates/idlemax-core/src/{types,protocol,quota}.rs`; this page describes the wire format and the semantics around them.
All bodies are JSON.
Every serde enum is `snake_case`.
Optional fields may be omitted on input; missing fields take their defaults.

## Auth and errors

Every call except `GET /healthz` and `GET /` needs `Authorization: Bearer <token>`.
The token comes from `IDLEMAX_TOKEN` or `$IDLEMAX_HOME/token` (mode 0600).
Any non-2xx response has the body `{"error": "..."}` (`protocol::ApiError`).

## Coordinator (default `127.0.0.1:7878`, tailnet IP in production)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | /healthz | | `ok` (text, no auth) |
| GET | / | | dashboard HTML (no auth) |
| POST | /v1/agent/heartbeat | `Heartbeat` | `HeartbeatReply` |
| POST | /v1/agent/job-update | `JobUpdate` | `JobUpdateReply` |
| POST | /v1/agent/advisor-inputs | `AdvisorInputs` | `{}` |
| GET | /v1/status | | `ClusterStatus` |
| GET | /v1/jobs?status=&limit= | | `[Job]` |
| GET | /v1/jobs/{id} | | `Job` |
| POST | /v1/jobs | `JobSpec` | `Job` |
| POST | /v1/jobs/{id}/cancel | | `Job` |
| GET | /v1/jobs/{id}/why | | `Explanation` |
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
`POST /v1/jobs` returns 201; it answers 400 for a spec that fails `JobSpec::validate` or names an `ai_account` missing from `[[quota.accounts]]`.
`POST /v1/jobs/{id}/cancel` answers 404 for an unknown job and 409 for a finished one.
`GET /v1/jobs/{id}/why` answers 404 for an unknown job (see [Why a job is not running](#why-a-job-is-not-running)).
A job update for an unknown job is 404.
`coordinator.listen` on `0.0.0.0` or `[::]` is refused at startup.

## Agent local API (default `127.0.0.1:7879`)

| Method | Path | Body | Returns |
|---|---|---|---|
| GET | /healthz | | `ok` (no auth) |
| GET | /v1/local/status | | `LocalStatus` |
| POST | /v1/local/control | `ControlRequest` with `global` scope; `pause`, `resume` or `stop_all` | `LocalStatus` |

It keeps working while the coordinator is down.
`POST /v1/local/control` waits up to 5 s for `local_paused` to reflect the action, so the agent publishes status through a `tokio::sync::watch` channel.
It returns 200 once applied, 202 when the agent has not applied it within 5 s, 400 for a non-global scope or `cancel`, and 503 when the agent loop is gone.
Creating `$IDLEMAX_HOME/PAUSE` pauses every job on the device without any API call; deleting it resumes at once.

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
A lease lasts `coordinator.lease_secs` (default 120 s) and is renewed by every heartbeat that reports the job in `running`.
An expired lease re-queues the job with `not_before_ms` backoff; after `max_attempts` it is `parked`.
AI jobs (`cost = "ai"`) get one attempt and never retry automatically.
A device is offline after `coordinator.offline_after_secs` (default 90 s) without a heartbeat.

### Job updates

The agent posts a `JobUpdate` on every status change of a leased job:

| Status | Sent by | When |
|---|---|---|
| `running` | agent | after a successful start or resume |
| `paused` | agent | after a pause |
| `queued` | agent | lease return: it could not start an assigned job (guard, kill switch, local pause, slots or RAM budget, already running); `error` says why |
| `failed` | agent | the runner could not start the job |
| `succeeded`, `failed`, `killed` | runner via agent | the process ended, including cap and max-runtime kills |

A `queued` update gives the lease back: the coordinator re-queues the job without counting an attempt.
Terminal updates carry `exit_code`, `output_path` (inbox file on the device) and `output_tail` (last 64 KiB of the attempt's log).
`rate_limited = true` means an AI job hit a usage limit or HTTP 429; that account is blocked for the rest of its window.
`owner_stop = true` marks a `killed` update caused by the owner's local `stop_all`.
An owner stop (coordinator or local `stop_all`) is final: the job ends `killed` and is not retried, while a guard kill is re-queued like any failed attempt.
An update never moves a terminal job out of its terminal status, and an update from a device that does not hold the lease is ignored.

Delivery is at least once and application is exactly once.
The agent appends each update to `agent/outbox.jsonl` (fsynced) before it sends it, in order, and drops it once the coordinator acks its `seq`.
After a crash or restart it replays whatever was never acked, so a `SIGKILL` between a job's exit and the ack still delivers that update.
Each update carries `seq`, a per-device number that is unique across restarts (it starts at the wall clock in milliseconds and the journal keeps it rising).
The coordinator stores `(device, seq)` in the same transaction as the job state, its event and any quota effect, and a replay of a stored `seq` changes nothing and is acked again.
The reply is `{"acked_seq": <seq>}`; `seq` 0 (an agent without the outbox) is applied without deduplication and answered without an ack, and an old coordinator's `{}` counts as an ack of the update just sent.
A stale non-terminal update after a terminal one never reopens the job.
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
 "reasons": [{"kind": "backoff", "message": "waiting out the retry delay: attempt 2 of 3 starts in 1m 10s (last error: exit code 1)",
              "fix": null, "until_ms": 1730000070000}],
 "devices": [{"device": "my-laptop", "online": true, "last_seen_ms": 1730000000000, "ready": false,
              "reasons": [{"kind": "guard", "message": "on battery (guard pause)",
                           "fix": "plug in, or set guard.require_ac_power = false", "until_ms": null}]}],
 "at_ms": 1730000001000}
```

- `state`: `ready` (a device leases it on its next heartbeat), `blocked` (queued or parked, with reasons), `active` (leased, running or paused) or `finished`.
- `reasons` hold on every device (pauses, retry backoff, AI quota, a busy AI account, no online device).
  `devices[].reasons` hold for that device only; `ready` is true when the device has none and neither does the job.
- `kind` is a stable snake_case code for scripts: `global_pause`, `account_pause`, `job_pause`, `device_pause`, `local_pause`, `backoff`, `parked`, `no_device_online`, `offline`, `device_filter`, `missing_capability`, `cost_class`, `command_not_allowed`, `ai_no_account`, `ai_account_not_on_device`, `ai_quota`, `ai_account_busy`, `guard`, `no_slot`, `ram_budget`, `queue_order`.
  `message` is plain language, `fix` is the command or config key that clears it (null when it clears by itself), `until_ms` is set for `backoff`.
- A `finished` job has no reasons, and a `parked` job has one `parked` reason carrying its last error.

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
