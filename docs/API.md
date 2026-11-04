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
| POST | /v1/agent/job-update | `JobUpdate` | `{}` |
| GET | /v1/status | | `ClusterStatus` |
| GET | /v1/jobs?status=&limit= | | `[Job]` |
| POST | /v1/jobs | `JobSpec` | `Job` |
| POST | /v1/jobs/{id}/cancel | | `Job` |
| GET | /v1/quota | | `[quota::QuotaView]` |
| POST | /v1/control | `ControlRequest` | `ClusterStatus` |
| GET | /v1/events?limit= | | `[Event]` |
| GET | /v1/advice | | `[Recommendation]`, open ones first |
| POST | /v1/advice/refresh | | `[Recommendation]` after re-running the advisor |
| POST | /v1/advice/{id}/accept | | `Job` queued from the suggested job |
| POST | /v1/advice/{id}/dismiss | | `Recommendation` |

`status` is a `JobStatus` (`queued`, `leased`, `running`, `paused`, `succeeded`, `failed`, `killed`, `cancelled`, `parked`).
`POST /v1/jobs` returns 201; it answers 400 for a spec that fails `JobSpec::validate` or names an `ai_account` missing from `[[quota.accounts]]`.
`POST /v1/jobs/{id}/cancel` answers 404 for an unknown job and 409 for a finished one.
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

Delivery is at least once: the agent queues updates in order and retries until it gets an answer, so the same update can arrive twice.
Applying an identical update twice is a no-op, and a stale non-terminal update after a terminal one never reopens the job.
The coordinator answers 4xx (other than 401, 403, 408 and 429) only when an update can never succeed, such as an unknown job id or a job leased to another device; the agent then drops it.
Auth failures, timeouts and 5xx keep the update queued for retry.
The agent sends heartbeats from their own task, so a slow coordinator delays heartbeats but never local controls or guard actions.

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

`Recommendation.key` is a stable dedup key (for example `parked_job:42`), so a refresh does not repeat advice the owner already saw.
Accepting queues `suggested_job`; nothing runs on the advisor's word alone, except job names listed in `advisor.auto_accept`.
Suggested jobs come from templates in `$IDLEMAX_HOME/jobs/*.toml`.
