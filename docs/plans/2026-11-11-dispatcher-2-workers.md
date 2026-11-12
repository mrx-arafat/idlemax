# Dispatcher 2: Launch, Verify, Stop Implementation Plan

Created: 2026-11-11
Agent: Claude Code
Status: IN_PROGRESS
Approved: Yes
Iterations: 1
Worktree: No
Type: Feature

## Summary

**Goal:** In `auto` mode idlemax launches each planned worker in its own git worktree and Herdr pane, checks every finished task itself and gives it a verdict, and lets the owner stop everything in seconds, continue the same sessions later, and keep or discard each result.

This plan builds roadmap item 34 and the decisions D3 (`auto` mode), D6, D8, D9, D11, D13 (one at a time), D14 (carrying out shed and emergency), D15, D16, D22 (the soft fences of A13), and from D10 item 1 the run record and the measuring week with its 10 percent cap (A7).

## Out of Scope

- Picking projects and tasks, the line state, the orchestrator pass, `rules.md` and the check job on the base branch belong to Spec 1 (`idlemax_core::dispatch` and `crates/idlemax/src/dispatch/`); this plan uses them by the names listed under Context for Implementer.
- The Budget, the tiers and the rest of an account after a rate limit belong to Spec 3; this plan uses the simple rule through Spec 1's `account_uses`, reports the text that ended a turn without a commit (A9), and offers one operation that stops the workers of one account.
- The dashboard, the TUI, notifications, `idlemax mcp` and the lock on the control API belong to Spec 4; this plan gives the Run its `reason` (A5) and passes `IDLEMAX_JOB_ID` into the pane (A12).
- The headless host, Linux, the device switch and the credential per device belong to Spec 5; this plan defines the seam that host will implement.
- A sandbox or a separate OS user for workers is not built here; the owner's answer is pending (A13, Open Question 1).
- OpenCode and agy workers are not built here; workers start with Claude and Codex (A21).
- A branch stacked on unreviewed work is not built here (A24).
- idlemax never executes a check that a model wrote; the Task's done check goes to the worker and the reviewer as text only.

## Approach

**Chosen:** A Run record on the coordinator, plus one ordinary job per worker launch that carries a new `worker` section in its `JobSpec` and is run on the agent by a worker manager behind two seams: `WorkerHost` for the pane and `Commands` for every child process.

**Why:** The existing job lifecycle already gives a lease renewed by heartbeats, journaled updates applied exactly once, level-triggered kill commands, the pause scopes, `stop-all`, the `PAUSE` kill switch, events and `idlemax jobs`, so workers inherit all of it and D11's "keep working and apply to workers too" needs no second protocol.
The cost is a short list of exceptions for worker jobs (listed below), and a Run record for what outlives a process: the verdict, the session id, the quota readings, the reason for the pick and the owner's review.

## Context for Implementer

A worker job is created by the coordinator already leased to one device, so the scheduler's `pick` never sees it.
The agent receives it through `HeartbeatReply.assign`, because `assign_jobs` re-sends every `leased` job of the device that the device does not report yet (`crates/idlemax/src/coord/api/agents.rs:181-191`).
Each launch is one job: the first start, each relaunch after a stop or a shed, and each keep or discard are separate jobs of the same Run.
"State saved" always means two things: the session id is already stored on the Run, and the worktree with its uncommitted edits is left untouched.

A worker job differs from a plain job in exactly these places, and nowhere else:

| Rule for plain jobs | Rule for worker jobs | Built in |
|---|---|---|
| One AI lease per account, one AI job start per 5 hours (`schema.rs:60-62`, `scheduler/blockers.rs:114-119`, `quota/rules.rs:306-311`) | Outside those rules in both directions (A8) | Task 2 |
| A returned lease is queued again, a retryable end is retried or parked (`scheduler/update.rs:44-60`) | The job ends, and the Run decides what happens next | Task 3 |
| A child process of the agent (`runner/launch.rs`) | A process in a pane, found through its member pids and the attempt tag | Tasks 5 and 6 |
| Started within the guard's slots and RAM, paused or killed by the guard verdict (`reconcile/mod.rs:175-187`) | Started by the line state, never paused by the guard, stopped by the shed and emergency lines (A16) | Task 7 |
| `command` is the argv the runner starts | `command` is the marker `["idlemax-worker", "<cli>"]`; the agent builds the real arguments | Tasks 5 and 9 |
| Held by every pause | A `launch` or `resume` is held like a job; `keep`, `keep_merge` and `discard` are never held (A10) | Tasks 7 and 11 |

What this plan uses from Spec 1, by its real name:

| Thing | Symbol and path |
|---|---|
| The mode | `Mode` in `crates/idlemax-core/src/dispatch/mod.rs`; changed with `POST /v1/dispatcher/mode` and `idlemax mode` (A1) |
| The line state | `LineState` and `RoomDecision { state, reason, open_slots, at_ms }` in `crates/idlemax-core/src/dispatch/room.rs`, kept by the agent as `Agent.room` after every sample and sent as `Heartbeat.room` |
| The project | `Project` and `ProjectSettings { base_branch, setup_command, check_command, off_limits, priority }` in `crates/idlemax-core/src/dispatch/project.rs`, plus `core_paths` (A4); read with `Store::get_project` |
| The task | `Task { id, project, project_id, device, source, title, done_check, needs_you }` in `crates/idlemax-core/src/dispatch/task.rs`, plus `scope_paths` (A4) |
| The plan | `Plan`, `DevicePlan { device, room, held, picks, empty }` and `Pick { project, task, title, provider, account, model, reason, by, avoid_paths }` in `crates/idlemax-core/src/dispatch/plan.rs`, gathered by `gather_plan` in `crates/idlemax/src/dispatch/mod.rs` |
| The simple account rule | `account_uses(cfg, views, paused, spent_week_pct)` in `crates/idlemax-core/src/dispatch/accounts.rs` |
| The rules text | `rules_file::read` in `crates/idlemax/src/dispatch/rules_file.rs` |
| The check on the base branch | `BaseCheck { project, base_commit, passed, .. }` in `crates/idlemax-core/src/dispatch/check.rs`, the result of Spec 1's check job (A11) |
| The refusal of dispatcher-only fields | `ControlError::DispatcherOnly` in `crates/idlemax-core/src/control/mod.rs` |
| The memory one worker counts for | `dispatcher.worker_mem_mib` in `crates/idlemax-core/src/config/dispatcher.rs` |

The fences, stated plainly (D22; every one is a soft fence until the owner answers A13):

| Fence | How | Proof |
|---|---|---|
| No personal MCP servers | `--strict-mcp-config` for Claude, one `-c mcp_servers.<name>.enabled=false` per configured server for Codex | Unit test on the arguments (Task 5), process row in `docs/WORKER-PROOF.md` |
| No idlemax token and no credential variable in the environment | Known names and the names a first probe found are blanked when the pane is made, then the agent's real environment is read and checked before the brief is sent; a name that survives fails the launch | Unit test of `env_problems` (Task 5), manager test that such a worker is never briefed (Task 6) |
| No untracked project files such as `.env` | A fresh worktree holds tracked files only | Worktree test (Task 4) |
| Codex file writes and network | Codex's own `workspace-write` sandbox with approval `never`; the repository's git directory is writable too, its hooks and config included | `docs/WORKER-PROOF.md` |
| `git push` from the worktree | git settings injected through the environment: a dead push address for every remote, no credential helper, no prompt; it blocks the plain command and can be undone on purpose | Test with a local bare remote (Task 5) |
| The project's setup and check commands | Run by idlemax with the same environment pairs as the pane, so code a worker wrote meets the same push block | Manager test (Task 6) |
| Pull requests, deploys, databases, messages, spending | Instructed by the brief and `rules.md`, backed by the missing environment credentials | `docs/WORKER-PROOF.md` |
| Credentials on disk or in the keychain, the idlemax token file and store | Not fenced | Open Question 1 |

## Runtime Environment

Every test and every manual check runs in a throwaway home on spare ports, never against `~/.idlemax`.

```sh
REAL_HOME=$HOME
T=$(mktemp -d) && export IDLEMAX_HOME="${T:?}/state" HOME="${T:?}/home" && mkdir -p "$IDLEMAX_HOME" "$HOME"
unset CLAUDE_CONFIG_DIR CODEX_HOME
test -n "$IDLEMAX_HOME" && test "$IDLEMAX_HOME" != "$REAL_HOME/.idlemax" && test -d "$IDLEMAX_HOME" || exit 1
target/debug/idlemax init
```

- **Ports:** point `coordinator.listen`, `coordinator.url` and `agent.listen` at the loopback ports 17878 and 17879 with `idlemax config set` before starting anything.
- **Never in a dev run:** an entry in `[[quota.accounts]]`, a real `claude`, `codex` or `herdr` call, `launchctl`, `systemctl`, or a project path inside a repository the owner is working in.
  Tests use `FakeHost` and `FakeCommands`, and a repository made with `git init` inside `$T`.
- **Task 12 only:** the real `HOME` stays, because the agent CLI needs its login, and `IDLEMAX_HOME` is still the temp one.
- **Stop:** end both processes and delete `$T`.

## Assumptions

- Spec 1 has landed with the names in the table above, with `scope_paths` on `Task` and `core_paths` on `ProjectSettings` (A4), and with argv setup and check commands (A4) - Tasks 6, 7, 8 and 9 depend on this.
- Spec 1's Plan fills a slot only from an orchestrator pass (A19), so every `Pick` this plan launches has `by` equal to `PickedBy::Orchestrator` - Task 9 depends on this, and checks it.
- Spec 1's `WorkerRoom::evaluate` reads `hold` while the guard would hold for a battery under its floor or for thermal throttling (A17), so no launch needs its own power rule - Tasks 7 and 9 depend on this.
- Herdr behaves as observed on version 0.9.4: every `herdr` subcommand prints JSON of the form `{"id": ..., "result": {...}}`, and a pane's processes inherit the variables given with `--env` - Tasks 5 and 12 depend on this, and Task 12 proves it.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A worker that ignores its brief reads a credential from disk and pushes or deploys | Low | High | No worker starts until Task 12 passed for its CLI, `workers.clis` is empty by default, Codex runs in its own sandbox, the gap is written in the README safety section, and Task 12 runs only with the owner's OK for the soft fences (A13) |
| Herdr changes its JSON, hangs or is not running | Medium | High | The Herdr host reads only named fields and every call has a 10 s limit; a start it cannot confirm is not briefed, and a stop never waits for Herdr: the saved member pids are killed directly (Task 6) |
| A worker's processes survive a stop and keep using memory | Low | High | The member pids are taken before the pane is closed, the terminal update is sent only after that set and the tagged set are empty, and what is left after 5 s is killed by pid (Task 6) |
| The same task is launched again and again | Medium | Medium | A task with a Run in any state except `discarded` is never launched, two failed Runs keep it off the plan, and a Run that fails to start three times becomes `needs_you` (Tasks 3 and 9) |
| Specs written in parallel claim the same migration number or config section | High | Low | Task 2 expects `V9` after Spec 1's `V8` and tells the implementer to check `main` first; this plan keeps its settings in its own `[workers]` section |

## File Structure

- `crates/idlemax-core/src/worker/mod.rs` (create, pure) - domain root for AI workers: `WorkerCli`, `WorkerAction`, `WorkerSpec`, `Reviewer`.
- `crates/idlemax-core/src/worker/run.rs` (create, pure) - `Run`, `RunState`, `Verdict`, the allowed state moves, `after_job`.
- `crates/idlemax-core/src/worker/report.rs` (create, pure) - `WorkerReport`, `WorkerPhase`, `CheckFacts`, `DiffStat`, `ReviewFacts`.
- `crates/idlemax-core/src/worker/name.rs` (create, pure) - task slug, branch, folder, pane name and tab label.
- `crates/idlemax-core/src/worker/launch.rs` (create, pure) - `agent_args`, `pane_env`, `env_problems`, `reviewer_args`.
- `crates/idlemax-core/src/worker/brief.rs` (create, pure) - the brief, the start line and the resume line.
- `crates/idlemax-core/src/worker/checks.rs` (create, pure) - `scope_findings`, `risk_is_high`, `verdict`.
- `crates/idlemax-core/src/worker/pace.rs` (create, pure) - `may_launch`, the week's spend, the measuring week, the review cap, which task is open.
- `crates/idlemax-core/src/worker/control.rs` (create, pure) - `plan_stop`, `plan_start`.
- `crates/idlemax-core/src/config/workers.rs` (create, pure) - `WorkersConfig`, its defaults and `validate`.
- `crates/idlemax-core/src/reconcile/workers.rs` (create, pure) - the worker start rule, the shed order and its latch, the emergency.
- `crates/idlemax-core/src/reconcile/worker_tests.rs` (create) - tests of the worker rules in the tick.
- `crates/idlemax/src/coord/store/runs.rs` (create, adapter) - Run rows and the owner stop row.
- `crates/idlemax/src/coord/api/runs.rs` (create, adapter) - run routes, stop and start, keep and discard.
- `crates/idlemax/src/coord/api/worker_tests.rs` (create) - coordinator tests of worker jobs and Runs.
- `crates/idlemax/src/coord/dispatch.rs` (create, adapter) - the `auto` step: `launch_due`, `stop_workers`.
- `crates/idlemax/src/agent/worker/mod.rs` (create, adapter) - the worker manager `Workers`.
- `crates/idlemax/src/agent/worker/cmd.rs` (create, adapter) - `Cmd`, the `Commands` seam, `OsCommands`, `FakeCommands`.
- `crates/idlemax/src/agent/worker/worktree.rs` (create, adapter) - worktrees with plain git.
- `crates/idlemax/src/agent/worker/host.rs` (create, adapter) - the runner seam `WorkerHost` and `FakeHost`.
- `crates/idlemax/src/agent/worker/herdr.rs` (create, adapter) - the Herdr host.
- `crates/idlemax/src/agent/worker/steps.rs` (create, adapter) - the phases of one launch.
- `crates/idlemax/src/agent/worker/record.rs` (create, adapter) - the saved worker record for a restarted agent.
- `crates/idlemax/src/agent/worker/check.rs` (create, adapter) - gathering the check facts and running the reviewer.
- `crates/idlemax/src/agent/worker/review.rs` (create, adapter) - keep, merge, discard.
- `crates/idlemax/src/agent/event_loop/workers.rs` (create, adapter) - the manager's task, its command channel and the worker tick.
- `crates/idlemax/src/cli/workers.rs` (create, delivery) - `stop`, `start`, `review`.
- `crates/idlemax/tests/workers.rs` (create) - the commands against a real coordinator in a temp home.
- `docs/WORKER-PROOF.md` (create) - the written manual check of each CLI, each fence and each reviewer.

## Progress Tracking

- [x] Task 1: Add the Run, its states, the worker job section and the `[workers]` settings to the core
- [x] Task 2: Store runs, list them, and take worker jobs out of the old AI rules
- [x] Task 3: End a worker job instead of retrying it, and let its Run follow
- [x] Task 4: Name things, run child commands with a limit, and manage worktrees with plain git
- [x] Task 5: Build the runner seam, the Herdr host and the fenced launch
- [x] Task 6: Run one worker job on the agent from lease to exit
- [x] Task 7: Start, hold, shed and stop workers in the agent tick
- [ ] Task 8: Check a finished task and give it a verdict
- [ ] Task 9: Launch from the Plan in auto mode, one worker at a time
- [ ] Task 10: Add idlemax stop and idlemax start
- [ ] Task 11: Add idlemax review with keep, merge and discard
- [ ] Task 12: Needs the owner's OK: prove each CLI and the fences on the Mac, then switch on auto

> Source of truth for completion: `spec-implement` toggles `[ ]` to `[x]` here.

## Implementation Tasks

### Task 1: Add the Run, its states, the worker job section and the `[workers]` settings to the core

**Objective:** Define in `idlemax-core` the Run record, the ten run states with their allowed moves, the verdict, the `worker` section that turns an ordinary job into one worker launch, and the `[workers]` settings.
Nothing uses them yet; this task fixes the names that the later tasks and the other specs build on.

**Files:**

- Create: `crates/idlemax-core/src/worker/mod.rs`
- Create: `crates/idlemax-core/src/worker/run.rs`
- Create: `crates/idlemax-core/src/worker/report.rs`
- Create: `crates/idlemax-core/src/config/workers.rs`
- Modify: `crates/idlemax-core/src/lib.rs`
- Modify: `crates/idlemax-core/src/config.rs`
- Modify: `crates/idlemax-core/src/config/settings.rs`
- Modify: `crates/idlemax-core/src/config/reload.rs`
- Modify: `crates/idlemax-core/src/types/job.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Modify: `crates/idlemax-core/src/spec_limits.rs`
- Modify: `crates/idlemax-core/src/control/mod.rs`
- Create: `crates/idlemax/src/coord/api/worker_tests.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `docs/API.md`
- Modify: `README.md`

**Key Decisions / Notes:**

- `Run` carries the shared fields of `docs/DISPATCHER.md`: id, device, project, task, branch, worktree, provider, account, model, session id, state, start and end time, quota before and after, verdict and concern (D10 item 1).
  The quota readings are two lists of the existing `QuotaSnapshot` (`types.rs:61`), so each window keeps its reset time and the time it was read.
  It also carries `reason`, copied from the Plan slot at launch (A5), the task title, the task source as the "task type" of D10 item 1 (A24), a one-line summary, the live or last `job`, `launches` (the jobs started for it) and `start_tries` (launches in a row that never reached `running`).
- `RunState` has exactly the ten shared values in `snake_case`, and `RunState::may_become(self, next) -> bool` allows only these moves: `starting` to `running`, `stopped`, `shed`, `needs_you`; `running` to `checking`, `stopped`, `shed`, `needs_you`; `checking` to `clean`, `needs_you`, `rejected`, `stopped`, `shed`; `stopped` to `shed` (the owner's start) and `discarded`; `shed` to `starting` and `discarded`; `clean`, `needs_you` and `rejected` to `kept` and `discarded`; `kept` and `discarded` to nothing (D11, D14, D16).
- `Verdict` is `clean`, `needs_you` or `rejected` (D8), kept beside the state so it stays visible after a keep or a discard.
- `JobSpec` gains `worker: Option<WorkerSpec>`, left out of the JSON when unset.
  `WorkerSpec` holds: `run`, `action` (`launch`, `resume`, `keep`, `keep_merge`, `discard`), `project`, `task` (the slug), `task_title`, `done_check`, `repo`, `base_branch`, `branch` and `worktree` (set for every action except a first `launch`), `cli` (`claude`, `codex`, `opencode`, `agy`), `model`, `session`, `setup` and `check` (argv lists, A4), `baseline` (a base commit and whether the check passed on it, when Spec 1 knows it), `scope_paths`, `off_limits`, `core_paths`, `reviewer` (cli, model, account), `review_diff_lines`, `blocked_secs`, `setup_secs` and `check_secs`.
  The brief travels in the existing `JobSpec.stdin`, which is already kept out of argv, operator responses (`coord/api/jobs.rs:123-126`) and the pid file (`runner/pidfile.rs:65`).
- `JobSpec::validate` refuses a `worker` section unless `cost` is `ai` (A8), and `spec_limits::check` bounds its paths and lists like `cwd` and `requires` (`spec_limits.rs:49-63`).
- `admit_job` (`control/mod.rs:86`) refuses every spec with a `worker` section through `ControlError::DispatcherOnly`, the variant Spec 1 adds for `admission = "lines"`.
  This task gives that variant the name of the refused field, so the message reads `worker is set by the dispatcher only`; its mapping to 400 sits in `coord/api.rs:147-158`.
  `admit_job` guards `POST /v1/jobs`, advice accept and schedules (`coord/api/jobs.rs:170`, `coord/api/advice.rs:161`, `coord/api/schedules.rs:114`, `coord/schedule.rs:36`), and the strict file parser already refuses the unknown key (`cli/job.rs:223`).
- `JobUpdate` (`protocol.rs:125`) gains `worker: Option<WorkerReport>`, left out when unset: the run id, the phase (`preparing`, `running`, `checking`, `done`), the session id, the branch, the worktree and, on the last update of a launch, the `CheckFacts` and `end_text`.
  `end_text` is the text that ended a turn in which no commit was added, at most 2,000 characters; it is the only output of a worker that leaves the device, and Spec 3 classifies it (A9).
- `WorkersConfig` is a `#[serde(default)]` struct added to `Config` as `pub workers: WorkersConfig`: `clis` (default empty), `reviewers` (empty), `review_cap` (3), `settle_secs` (60), `max_runtime_secs` (3600), `blocked_secs` (120), `review_diff_lines` (300), `setup_secs` (900) and `check_secs` (600) (A25).
  `validate` refuses a `review_cap` of 0, a time of 0, and a `clis` or `reviewers` entry other than `claude` or `codex`; `Config::validate` (`config.rs:160`) calls it and puts the key name first.
  `"workers"` joins `SECTIONS` in `config/settings.rs:12`, so `idlemax config` lists, gets and sets the keys.
  In `plan_reload` (`config/reload.rs:50-70`) every `workers.*` key goes to `applied`, because the coordinator reads `[workers]` fresh at every launch step (A2, Task 9) and carries the values a device needs in the `WorkerSpec`.
- `coord/api.rs` gains `#[cfg(test)] mod worker_tests;`; new coordinator tests of this plan go there, since `coord/api/tests.rs` already has 1123 lines.
- `README.md`: the Configuration table gains the nine `workers.*` rows.

**Definition of Done:**

- [x] A test walks every pair of states and `may_become` allows exactly the moves listed above.
- [x] A spec with a `worker` section and a `cost` other than `ai` fails `validate`, `admit_job` refuses any spec with a `worker` section, and the coordinator test `worker_spec_is_refused_from_the_api` gets 400 from `POST /v1/jobs` with a message that says only the dispatcher sets it.
- [x] A `JobSpec` and a `JobUpdate` without the new fields serialize with no `worker` key, and one with them survives a JSON round trip.
- [x] `Config::parse("[workers]\nreview_cap = 0\n")` fails with a line that starts with `workers.review_cap`, an empty file parses with `clis` empty and `review_cap` 3, and `plan_reload` puts a changed `workers.review_cap` under `applied`.
- [x] `docs/API.md` describes `JobSpec.worker` and `JobUpdate.worker`.
- [x] Verify: `cargo test -p idlemax-core worker && cargo test -p idlemax-core control && cargo test -p idlemax-core config && cargo test -p idlemax --bin idlemax worker_tests && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 2: Store runs, list them, and take worker jobs out of the old AI rules

**Objective:** Give the coordinator a `runs` table, the store methods that read and change a Run safely, and two read routes, so every later step has one place to record what a worker did.
The same task takes worker jobs out of the one-AI-lease rule and the one-start-per-5-hours rule in both directions (A8).

**Files:**

- Modify: `crates/idlemax/src/coord/store/schema.rs`
- Create: `crates/idlemax/src/coord/store/runs.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/coord/store/jobs.rs`
- Modify: `crates/idlemax-core/src/scheduler/blockers.rs`
- Create: `crates/idlemax/src/coord/api/runs.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `docs/API.md`
- Test: `crates/idlemax-core/src/scheduler/tests.rs`

**Key Decisions / Notes:**

- The migration is expected to be `V9`: `MIGRATIONS` ends at `V7` today (`schema.rs:4`) and Spec 1 adds `V8`.
  Specs written in parallel may claim the same number, so the implementer checks `main` first and takes the next free one.
- The migration adds the `runs` table: the fields of `Run`, a `state` and a `verdict` column each with a `CHECK` on its values, `reason`, `launch` (JSON: the `WorkerSpec` and the brief a relaunch needs), `facts` (JSON), `quota_before` and `quota_after` (JSON lists), `job`, `launches`, `start_tries`, `start_ms`, `end_ms` and `updated_ms`, with indexes on `(state, device)`, `(project, state)` and `(account, start_ms)`.
  It adds `worker_stop`, a table that holds at most one row (`stopped_ms`, `reason`): the owner's stop, used by Task 10.
- It adds `jobs.worker INTEGER NOT NULL DEFAULT 0`, drops `jobs_one_ai_lease` and creates it again with `AND worker = 0` in its `WHERE` clause, so several worker jobs may share an account and a plain AI job never collides with one.
- The three reads that feed the old AI rules leave worker jobs out (A8): `ai_leased_accounts` in `scheduler/blockers.rs:114-119` skips a job whose spec has a `worker` section, and `Store::ai_leased_accounts` (`store/jobs.rs:146-154`) and `Store::ai_job_starts` (`store/jobs.rs:102-109`) add `AND worker = 0`.
  Without this a worker on an account would block the advisor pass and Spec 1's orchestrator pass through `Blocker::AccountLeased` (`scheduler/blockers.rs:185-187`) and through "an AI job already started on this account in the last 5 h" (`quota/rules.rs:306-311`).
- `insert_job` and `update_jobs` (`store/jobs.rs:249`, `:171`) write `worker = 1` when the spec has a `worker` section.
  A new `insert_leased_job(spec, device, expires_ms, now)` inserts a job directly as `leased` to one device with `attempts = 1`.
  It runs the same key check as `insert_job_once` in one transaction (`unfinished_with_key`, `store/jobs.rs:45-56`, `:290-295`) and returns `Added::Existing` for a key that is still live, because the index on `key` is not unique (`schema.rs:157`).
- `update_run(run, expect_state)` is a compare-and-set on the stored state and returns false when the state moved, like `lease_job` does for jobs, so a late agent update never overwrites an owner's stop.
  `get_run`, `list_runs(state, project, limit)`, `runs_of_account(account, since_ms)` and `runs_of_project(project)` are the reads the later tasks use.
- `prune` (`store.rs:208`) also removes `kept` and `discarded` runs whose `end_ms` is older than `JOB_RETENTION_MS` (`store.rs:37`).
- `GET /v1/runs?state=&project=&limit=` answers newest first with `page_limit` (`coord/api.rs:164`), and `GET /v1/runs/{id}` answers 404 with `run <id> not found` for an unknown id; neither returns the `launch` column, so the brief never leaves the store.

**Definition of Done:**

- [x] A fresh store migrates to the new version, and a store at the version before it with one job row migrates with that row intact and `worker` 0.
- [x] Two worker jobs on one account can both be `leased`, a second plain AI job on an account a plain AI job holds still fails with `AccountLeased`, and a plain AI job is leased while a worker job holds the same account.
- [x] `pick` returns a queued plain AI job while a worker job is active on its account, and `ai_job_starts` leaves a started worker job out.
- [x] `insert_leased_job` with a key that a live job holds returns that job and inserts nothing.
- [x] `update_run` changes a run whose stored state matches and returns false, changing nothing, when it does not.
- [x] `GET /v1/runs` filters by state and project, and `GET /v1/runs/999` answers 404.
- [x] `docs/API.md` lists both routes, the `Run` shape and the ten states.
- [x] Verify: `cargo test -p idlemax-core scheduler && cargo test -p idlemax --bin idlemax store::runs && cargo test -p idlemax --bin idlemax api::runs && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 3: End a worker job instead of retrying it, and let its Run follow

**Objective:** Make a worker job always end when its attempt ends, and move its Run in the same transaction, so the Run alone decides whether the work is relaunched.
A queued worker job would be leased by the scheduler under the plain AI rules, and a parked one would raise advisor advice; neither may happen.

**Files:**

- Modify: `crates/idlemax-core/src/scheduler/update.rs`
- Modify: `crates/idlemax-core/src/scheduler/lease.rs`
- Modify: `crates/idlemax-core/src/worker/run.rs`
- Modify: `crates/idlemax/src/coord/store/updates.rs`
- Modify: `crates/idlemax/src/coord/store/controls.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax/src/coord/api/jobs.rs`
- Modify: `crates/idlemax/src/coord/sweep.rs`
- Modify: `docs/API.md`
- Test: `crates/idlemax-core/src/scheduler/tests.rs`
- Test: `crates/idlemax/src/coord/api/worker_tests.rs`

**Key Decisions / Notes:**

- In `apply_update` a worker job that returns its lease (`queued`) ends `cancelled`, and one that ends `failed` or `killed` with `retryable` ends `killed`; in `expire_leases` a worker job whose lease ran out ends `killed` (`scheduler/update.rs:44-60`, `scheduler/lease.rs:15-36`).
  A worker job is never `queued` again and never `parked`.
- `run::after_job(run, job, update, now_ms) -> Option<Run>` is pure and maps the updates of a `launch` or `resume` job; a job with another action leaves the Run unchanged in this task.
  A `running` update with phase `running` gives `running` and sets `start_tries` to 0, phase `checking` gives `checking`, and every update that carries a session id, branch or worktree stores it.
  A returned lease, a `retryable` end and an expired lease give `shed` (D14, "back in the queue").
  When such an end finds the Run in `starting` for the third time in a row (`start_tries` reaches `START_TRIES`, 3), the Run becomes `needs_you` with the concern `could not be started 3 times: <last error>`, so a launch that cannot start is not repeated without end.
  A `killed` update with `owner_stop` gives `stopped` (D11).
  A `failed` job, and a `killed` one that is neither, give `needs_you` with the job's error as the concern.
  A `succeeded` launch stays in `checking` until Task 8 gives it a verdict.
  `end_text` of the last update is stored in the Run's facts (A9).
- `UpdateWrites` (`store/updates.rs:24`) gains the changed Run, so the job, its event and its Run commit together in `commit_update` and a replayed update changes nothing twice.
- When a coordinator control ends a `launch` or `resume` job (`stop_all` or `cancel` in `apply_control`, `coord/api/jobs.rs:180`), its Run moves to `stopped` in the transaction of `Store::apply_control` (`store/controls.rs:11-26`), because the owner asked for it (D11).
  A control that ends a `keep`, `keep_merge` or `discard` job leaves its Run as it was.
- The sweep's `expire` (`sweep.rs:88`) moves the Run of each expired worker job to `shed`.
- A lease that ran out while the coordinator itself slept is renewed, not ended.
  `maintain` (`sweep.rs:41-58`) remembers when it last swept, as it does for the prune; when the gap is more than two sweep periods (`SWEEP_EVERY`, `sweep.rs:25`), `expire` gives every expired worker job a new deadline with `lease_deadline` (`scheduler/lease.rs:9`) in that one sweep.
  The coordinator and the agent share the Mac until Spec 5, the sweep timer does not count sleep and the lease is compared with the wall clock, so without this rule a sweep that fires before the first heartbeat after a sleep would end every worker, and each resume reads its whole session again.

**Definition of Done:**

- [x] A worker job that returns its lease is `cancelled`, a retryable kill is `killed`, an expired lease is `killed`, and in no case is the job `queued` or `parked`; the same updates on a plain job behave as before.
- [x] One test per row of the mapping above passes for `after_job`, and the third start that fails in a row gives `needs_you` with the last error in the concern.
- [x] A replayed job update leaves the Run unchanged, and an update for a run the owner already stopped does not move it.
- [x] `stop-all` on the account of a running worker leaves the job `killed` and the Run `stopped`.
- [x] A sweep that follows a gap of 60 s ends no worker job and renews its lease, and the next sweep 15 s later ends one whose lease then runs out.
- [x] `docs/API.md` "Retries" and "Job updates" say what differs for worker jobs.
- [x] Verify: `cargo test -p idlemax-core scheduler && cargo test -p idlemax-core worker::run && cargo test -p idlemax --bin idlemax worker_tests && cargo test -p idlemax --bin idlemax sweep && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 4: Name things, run child commands with a limit, and manage worktrees with plain git

**Objective:** Decide in one pure module how a task becomes a branch, a folder, a pane name and a tab label, give the agent one seam through which every child process runs, and the three git operations it needs: create a worktree, remove it safely, delete its branch.
idlemax touches only folders and branches it created itself.

**Files:**

- Create: `crates/idlemax-core/src/worker/name.rs`
- Modify: `crates/idlemax-core/src/paths.rs`
- Create: `crates/idlemax/src/agent/worker/mod.rs`
- Create: `crates/idlemax/src/agent/worker/cmd.rs`
- Create: `crates/idlemax/src/agent/worker/worktree.rs`
- Modify: `crates/idlemax/src/agent/mod.rs`

**Key Decisions / Notes:**

- A slug is lowercase, with every run of characters outside `[a-z0-9]` turned into one `-` and no `-` at either end.
  The task slug is the first 32 characters of the slugged Task id, a `-`, and the first 6 hex digits of the FNV-1a 64-bit hash of the whole id, computed in `name.rs`.
  Spec 1's ids are long (`github_issue:<owner/name>#<number>`, `session:<project id>:<tool>:<session id>`), and the hash keeps two tasks apart when the cut drops the part that differs.
- The branch is `idlemax/<task slug>` and the folder is `$IDLEMAX_HOME/worktrees/<project slug>/<task slug>` from a new `Paths::worktrees_dir()` (D15).
  When that branch or folder already exists, `-r<run id>` is appended to both, so a task tried again never reuses or stacks on an old branch (D9, A24).
- The pane name is `im-<project>-<task>-<run>`: the project slug cut to 10 characters, the task slug cut so the whole name has at most 32 characters, and the run id in base 36.
  The run id makes every name unique, and the result always matches `[a-z][a-z0-9_-]{0,31}`; the tab label is `im: <Project>` with the project's name as written (D15, Open Question 2).
- `cmd.rs` holds the seam for every child process the worker code starts:

  ```rust
  pub struct Cmd { pub program: String, pub args: Vec<String>, pub cwd: Option<PathBuf>,
                   pub env: Vec<(String, String)>, pub stdin: Option<String>,
                   pub limit: Duration, pub throttle: bool, pub tag: Option<u64> }
  pub struct Output { pub code: Option<i32>, pub stdout: String, pub stderr: String }
  pub trait Commands: Send + Sync { fn run(&self, cmd: &Cmd) -> Result<Output, CmdError>; }
  ```

  `OsCommands` is the real one: it starts the child in its own process group with the minimal environment of `job_run::inherits` (`job_run/env.rs:9-35`) plus `GIT_TERMINAL_PROMPT=0` plus `cmd.env`, sets `IDLEMAX_JOB_TAG` when `tag` is set, wraps the argv with the device's `Wrapper::wrap` (`job_run/wrap.rs:43`) when `throttle` is true (A16), and kills the group when the limit passes (`CmdError::TimedOut`).
  `FakeCommands` under `#[cfg(test)]` answers from a script and records every call; every test of this plan uses it, so no test starts `git` outside a temp repository, and none starts `herdr`, `claude` or `codex`.
  These calls run on the manager's own task (Task 7), never on the agent loop.
- Create runs `git -C <repo> worktree add -b <branch> <folder> <base branch>` with a 60 s limit and then reads the base commit with `git -C <folder> rev-parse HEAD`; every other git call has a 10 s limit.
  It never fetches, so nothing touches the network or a credential.
  Beside the folder it writes `<folder>.json` (mode 0600): run, repo, branch, base commit and creation time.
  That marker is the proof that idlemax created the folder (D15, D16).
- Remove runs `git -C <repo> worktree remove <folder>` with no force flag, only for a folder under `worktrees_dir()` that has its marker, and passes git's own message on when git refuses (D16).
  Branch delete runs `git -C <repo> branch -D <branch>` only when the name starts with `idlemax/` and equals the branch in the marker.

**Definition of Done:**

- [x] `pane_name` matches `[a-z][a-z0-9_-]{0,31}` for a long project name, a task id with spaces and symbols, and a run id above one million, and two runs of the same task get different names.
- [x] Two task ids that share their first 40 characters get different task slugs.
- [x] `OsCommands` returns `TimedOut` for a child that outlives its limit and leaves no process of its group, and the child's environment holds no `IDLEMAX_TOKEN` and nothing beyond `job_run::inherits`, `GIT_TERMINAL_PROMPT` and the given pairs.
- [x] In a temp repository, create gives a folder on a new `idlemax/` branch at the base commit, and an untracked `.env` in the owner's checkout is absent from it.
- [x] Remove refuses a folder with an uncommitted file and reports git's message, removes a clean one, and refuses a folder that has no marker or lies outside `worktrees_dir()`.
- [x] Creating the same task twice gives a second branch and folder that end in `-r<run id>`.
- [x] Verify: `cargo test -p idlemax-core worker::name && cargo test -p idlemax --bin idlemax agent::worker::cmd && cargo test -p idlemax --bin idlemax agent::worker::worktree && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 5: Build the runner seam, the Herdr host and the fenced launch

**Objective:** Define the one interface for "start a worker, read its state, send it text, stop it, read what ended its turn", implement it through the `herdr` command line, and decide in pure code what each agent CLI is started with, what it is told, and which environment it may see.
The written manual check that proves each fence is added here too.

**Files:**

- Create: `crates/idlemax-core/src/worker/launch.rs`
- Create: `crates/idlemax-core/src/worker/brief.rs`
- Create: `crates/idlemax/src/agent/worker/host.rs`
- Create: `crates/idlemax/src/agent/worker/herdr.rs`
- Modify: `crates/idlemax/src/agent/worker/mod.rs`
- Modify: `crates/idlemax/src/agent/runner/os.rs`
- Modify: `crates/idlemax/src/agent/runner/mod.rs`
- Create: `docs/WORKER-PROOF.md`

**Key Decisions / Notes:**

- `WorkerHost` is the runner seam: `start(&Launch) -> Handle`, `state(&Handle) -> HostState` (status, session id, the pane's shell pid, the foreground pids), `send(&Handle, text)`, `stop(&Handle)` and `end_text(&Handle) -> Option<String>`.
  It has a second implementation from the start, a `FakeHost` under `#[cfg(test)]` that Task 6 needs, and Spec 5 adds the headless one.
- The Herdr host runs every `herdr` call through `Commands` with a 10 s limit.
  It finds or creates the workspace labelled `idlemax workers` (`herdr workspace list`, else `herdr workspace create --label "idlemax workers" --cwd <folder> --env K=V --no-focus`, whose first tab is renamed with `herdr tab rename <tab_id> <label>`), then the project's tab (`herdr tab list --workspace <id>`, else `herdr tab create --workspace <id> --cwd <folder> --label "im: <Project>" --env K=V --no-focus`), else it splits a pane of that tab (`herdr pane split --pane <id> --direction down --cwd <folder> --env K=V --no-focus`) (D15).
  The new pane is the one `herdr pane list --workspace <id>` did not show before the call; it is labelled with `herdr pane rename <pane_id> <name>`, and the agent starts with `herdr agent start <name> --kind <kind> --pane <pane_id> -- <arguments>`.
- State comes from `herdr agent list` (`result.agents[]`: `pane_id`, `agent_status`, `agent_session.value`) and `herdr pane process-info --pane <pane_id>` (`result.process_info.shell_pid` and `result.process_info.foreground_processes[].pid`).
  The pane's `label` and `workspace_id` come from `herdr pane list` (`result.panes[]`), not from the agent list, which has no label.
  `send` is `herdr agent prompt <pane_id> <text>`; `stop` is `herdr pane close <pane_id>`; `end_text` is the last 40 lines of `herdr agent read <pane_id> --source recent --lines 40`, cut to 2,000 characters (A9).
  The host never passes `--focus`, never calls a `focus` subcommand, names its own workspace or pane in every call, and closes a pane only when its `workspace_id` and `label` still match what it saved (D4).
- Before the agent starts, the host puts the pane's shell into the device's background class, so a worker keeps the runner's OS throttle (A16): on macOS `/usr/sbin/taskpolicy -b -p <shell pid>`, with the binary the runner's wrapper already uses (`runner/wrapper.rs:20-34`) and the form its usage text lists as `taskpolicy [-b|-B] -p pid`.
  Task 6 does the same for every member pid it sees later, so the rule does not depend on a child inheriting the class (Open Question 5).
- `agent_args(cli, model, session, git_common_dir, mcp_servers)` returns the arguments after the program name.
  Claude: `--model <model> --strict-mcp-config --permission-mode bypassPermissions`, plus `--session-id <uuid>` for a first start and `--resume <session>` to continue (D10 item 9, D11).
  The uuid is made by the agent from the OS random source that `generate_token` reads (`state.rs:167`), so the session id is on the Run before the agent starts and a stop one second later can still be continued.
  It does not pass `--bare`, which would switch off the subscription login, and it keeps the user settings, because Herdr learns the agent's state through its integration there (Open Question 4).
  Codex: `--model <model> --sandbox workspace-write --ask-for-approval never --add-dir <git common dir>`, one `-c mcp_servers.<name>.enabled=false` for each entry of `mcp_servers`, and `resume <session>` first to continue.
  The git common directory is `git rev-parse --git-common-dir` of the worktree, because a commit writes objects and refs there.
  An empty `mcp_servers` table does not work: `codex -c 'mcp_servers={}' mcp list` still lists every configured server as enabled on codex-cli 0.166.0, while the per-server override shows it disabled.
  The agent reads the server names from the first column of `codex mcp list` through `Commands`; when that call fails the launch fails closed.
  OpenCode and agy return a typed `Unsupported` error that names the missing flag (A21).
- `pane_env(job_id, tag, remotes, blank)` returns the `--env` pairs: the existing `IDLEMAX_JOB_ID`, which is the worker marker (A12), and `IDLEMAX_JOB_TAG` (`runner/launch.rs:17-20`), `GIT_TERMINAL_PROMPT=0`, a `GIT_CONFIG_COUNT` block that sets `credential.helper` to empty and `remote.<name>.pushurl` to `idlemax-no-push://<name>` for each remote, and empty values for `IDLEMAX_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, `SSH_AUTH_SOCK` and every name in `blank` (D6, D22).
- `env_problems(vars) -> Vec<String>` names every variable with a non-empty value that is `IDLEMAX_TOKEN` or `SSH_AUTH_SOCK`, that starts with `AWS_`, that ends in `_TOKEN`, `_KEY`, `_SECRET`, `_PASSWORD` or `_CREDENTIALS`, or that contains `DATABASE_URL`; it returns names only, never values.
  A new `ProcTable::environ(pid)` in `runner/os.rs` reads a process's variables the way `read_tags` already does (`runner/os.rs:239`), and `runner/mod.rs` makes `os` visible to the agent's worker module.
- `brief(task, project, rules)` in `brief.rs` writes fixed sections in this order: the goal, the files in scope (`scope_paths`), what not to touch (the off-limits paths, any path outside the scope, the owner's own folder), the exact done check, "run focused tests only, never a whole suite, and add no test the task does not need", "commit locally on this branch; never push, open or merge a pull request, deploy, touch a database, send a message or spend money", and the text of `rules.md` (D6, D8).
  `start_line(path)` is the one line that tells the agent to read the brief file, and `resume_line(path)` the one that tells it to continue the task in that file (D11).
  One task gets one fresh session (D10 item 9).
- `docs/WORKER-PROOF.md` is the written manual check, with a step, the command and the expected result for each row.
  Before the tables: the names `env_problems` reports in a plain new pane (names only).
  One table per CLI: a start that does not take focus; a first start in a folder the CLI has never seen that reaches `idle` with no dialog; the session id; the environment check; a working commit, with a note on commit signing; a refused `git push`; no MCP helper process; the background class set on the shell and the sample task still finishing, with its time; `idlemax pause` then `resume`, and the `PAUSE` file, each freezing and thawing the same turn; a stop; a continued session; the memory of the worker's process set; the input tokens of the first turn and a turn that ends with the owner's hooks on; one run started by the installed service.
  One table per reviewer CLI: one real review that returns the JSON answer, with its cost.

**Definition of Done:**

- [x] `agent_args` returns the exact Claude lists above for a first start and for a continued session, the Codex list with one `enabled=false` override per given server name, and an `Unsupported` error for OpenCode and agy that names what is missing.
- [x] `env_problems` reports `IDLEMAX_TOKEN`, `GH_TOKEN`, `SSH_AUTH_SOCK`, `ANTHROPIC_API_KEY` and `DATABASE_URL` when set, and nothing for `PATH`, `HOME`, `TERM`, `HERDR_PANE_ID`, `IDLEMAX_JOB_ID` or a blank `GH_TOKEN`.
- [x] `pane_env` with `blank` of `["FOO_TOKEN"]` holds `FOO_TOKEN=` and `IDLEMAX_JOB_ID=<job id>`.
- [x] The Herdr host builds the commands above from fixture JSON in the observed shapes through `FakeCommands`, never emits `focus`, refuses to close a pane whose label or workspace differs from the saved one, and reports a start as failed when a call returns `TimedOut`.
- [x] With `pane_env` applied, `git push` from a worktree to a local bare remote fails and the remote has no new ref.
- [x] The brief of a sample task contains each fixed section, the scope paths and the task's done check.
- [x] Verify: `cargo test -p idlemax-core worker::launch && cargo test -p idlemax-core worker::brief && cargo test -p idlemax --bin idlemax agent::worker::herdr && cargo test -p idlemax --bin idlemax agent::worker::host && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 6: Run one worker job on the agent from lease to exit

**Objective:** Build the worker manager that carries one leased worker job through its phases: prepare the worktree, start the agent in its pane, check its environment, send the brief, watch it until its turn ends, and stop it with its state saved whenever it is told to.
The manager is tested with `FakeHost` and `FakeCommands`; Task 7 puts it into the agent loop.

**Files:**

- Create: `crates/idlemax/src/agent/worker/steps.rs`
- Create: `crates/idlemax/src/agent/worker/record.rs`
- Modify: `crates/idlemax/src/agent/worker/mod.rs`
- Modify: `docs/API.md`

**Key Decisions / Notes:**

- `Workers` owns the host, the `Commands`, a `ProcTable` and the tracked workers.
  It takes `Start(Job)`, `Pause(JobId)`, `Resume(JobId)` and `Kill { id, order }`, and `tick(now_ms, awake_ms)` advances every worker one step and returns the job updates; `running()` returns each worker as a `RunningJob` with the `rss_bytes` of its process set.
- A start is refused, and the job ends `failed` and final, when the worker CLI, the reviewer CLI or the first word of the setup or check argv is not in `agent.command_allowlist` (`job_run::allows_program`, `job_run/mod.rs:42`), or when the manager still tracks a worker of the same run.
  The runner checks the allowlist for plain jobs (`runner/mod.rs:158`); worker jobs do not pass through it, so the manager does.
- Phases of a `launch`: create the worktree, or reuse the folder whose marker names this run; run the setup argv; run the check argv and record its exit code as the baseline in the marker (D8); start the agent; check its environment; write the brief to `<folder>.brief.md` (mode 0600) and send `start_line`.
  Setup and check run through `Commands` with the `pane_env` pairs, the attempt tag, `throttle` true and the limits `setup_secs` and `check_secs` (A16), so they meet the same push block as the pane and a stop can find them.
  When the spec's `baseline` names the worktree's base commit, its result is the baseline and the check is not run a second time, because Spec 1's check job already ran it on that commit (A11).
  A check that times out, or a project with no check argv, records the baseline as not recorded.
  A `resume` skips setup and baseline, starts with the session id, and sends `resume_line` (D11).
- The environment check waits until the status is `idle`, at most 60 s, then reads the foreground process's variables with `ProcTable::environ` and runs `env_problems`.
  When it reports names, the pane is closed and made once more with those names in `blank`, which clears every name inherited from the Herdr server.
  When names survive the second start, or the environment cannot be read, the pane is closed, no brief is sent and the job ends `failed` and final; the message lists the names and says to guard each export in the shell start-up file with `[ -z "$IDLEMAX_JOB_ID" ]` (D22).
  The brief line is sent only while the status is `idle`, so it is never typed into a dialog.
- The turn is over when the Herdr status is `done`, or `idle` after it was `working`, on two ticks in a row; the pane is then closed.
  When no commit was added during this launch, the manager first takes `end_text` from the host and puts it into the last update (A9).
  A `blocked` status that lasts longer than `blocked_secs` stops the worker and ends the job `failed` with "the worker waited for input", and a pane that vanished ends it `failed` with "the agent exited before it finished".
- The manager keeps a `RunClock` (`job_run/mod.rs:68`) per worker, with awake time and with paused spans left out; when it passes `max_runtime_secs` the worker is stopped and the job ends `killed` and final with "ran longer than <n> s", which `after_job` maps to `needs_you`.
  The runner does this for plain jobs (`runner/stop.rs:19`), and a lease has no cap (`scheduler/lease.rs:6-8`).
- While a worker holds any pause, no end-of-turn rule, no `blocked_secs` rule and no runtime rule is applied.
  Pause and resume send `SIGSTOP` and `SIGCONT` with `os::signal_pids` (`runner/os.rs:98`) to the member pids, and during setup or check to the tagged child (A24: a pause freezes a worker in place).
- A stop never depends on Herdr.
  On every tick the manager saves the member pids with their start times: the shell pid and the `outside_group` of `ProcTable::usages` for a `JobKey` whose root is the shell pid and whose tag is the attempt tag (`runner/os.rs:193`, `job_run/usage.rs:66-73`).
  To stop, it sends `SIGCONT` to a paused worker, closes the pane, and then waits until none of the saved pids and none of `ProcTable::orphans(&key, false)` (`runner/os.rs:201-205`, found by tag) is alive.
  `usages` alone cannot do this, because it returns nothing once the root has exited.
  What is left after 5 s, and everything at once when `herdr pane close` fails or times out, is killed with `os::signal_pids`; a group signal is never used, because the shell's pid may be reused once it exited.
  The terminal update is sent only when the set is empty, so a Run is never relaunched over a live worker (D11, D14).
  A kill during setup or check kills the tagged child the same way.
- On every tick each member pid not yet throttled gets the background class of Task 5 (A16).
- `record.rs` saves each live worker under `agent/workers/<job id>.json` in `Paths::agent_state_dir()` (mode 0600): job id, attempt, run, phase, session id, pane, label, workspace, shell pid and its start time, and the tag.
  `Workers::recover` takes a saved worker back when its pane is still listed with the same label and workspace and its shell pid has the saved start time, and goes on watching it, so an agent restart costs no session reload.
  A saved worker that does not match has its pane closed when it matches by label, its tagged leftovers killed as `pidfile::reap_orphans` does for jobs (`runner/pidfile.rs:119`), and its job reported `killed` with `retryable`, so its Run is `shed` and resumes by itself.

**Definition of Done:**

- [x] With a `FakeHost` and a temp repository, a launch creates the worktree, records the baseline, sends the brief only after the environment check passed and the status was `idle`, reports the session id in its first update, and ends `succeeded` after the status goes `working` then `done` on two ticks.
- [x] A fake worker whose environment holds `GH_TOKEN` on the first start and not on the second is briefed after one restart; one that still holds it is never sent a brief and its job ends `failed` naming the variable and the fix.
- [x] A job whose CLI is missing from the allowlist ends `failed` and final without a pane, and so does one whose check argv starts with a program that is missing.
- [x] A kill closes the pane, leaves the worktree and an uncommitted file in place, and reports `killed` only after the fake process set is empty; a set that outlives the close is killed after 5 s, and a close that errors still ends with the set empty.
- [x] A kill during setup ends the fake child, and the setup and check calls carry the `pane_env` pairs, the tag and `throttle` true.
- [x] A worker past `max_runtime_secs` of awake time ends `killed` with "ran longer than"; a paused worker whose status reads `idle` is not ended and its clock does not move.
- [x] A turn that ends with no new commit puts the host's `end_text` into the last update, and one with a commit does not.
- [x] A `resume` job starts with the saved session id and does not run setup again.
- [x] A launch whose spec carries a `baseline` for the worktree's base commit does not run the check argv before the agent starts, and one whose `baseline` names another commit runs it once.
- [x] A manager built over a saved record takes back a worker whose pane matches and reports a retryable kill for one whose pane is gone.
- [x] Verify: `cargo test -p idlemax --bin idlemax agent::worker::steps && cargo test -p idlemax --bin idlemax agent::worker::record && cargo test -p idlemax --bin idlemax agent::worker::tests && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 7: Start, hold, shed and stop workers in the agent tick

**Objective:** Teach the pure agent tick the rules for worker jobs, and put the manager into the agent loop: a worker starts by the line state, is never paused by the guard, is shed one at a time until the device is back at `run`, and is stopped at once in an emergency.
The existing pause, resume, kill, stop-all and `PAUSE` paths reach workers through the same tick decisions as jobs.

**Files:**

- Create: `crates/idlemax-core/src/reconcile/workers.rs`
- Create: `crates/idlemax-core/src/reconcile/worker_tests.rs`
- Modify: `crates/idlemax-core/src/reconcile/mod.rs`
- Modify: `crates/idlemax-core/src/reconcile/starts.rs`
- Create: `crates/idlemax/src/agent/event_loop/workers.rs`
- Modify: `crates/idlemax/src/agent/event_loop.rs`
- Modify: `crates/idlemax/src/agent/event_loop/jobs.rs`
- Modify: `crates/idlemax/src/agent/event_loop/coord.rs`
- Modify: `crates/idlemax/src/agent/mod.rs`
- Modify: `crates/idlemax/src/agent/runner/os.rs`
- Modify: `README.md`
- Modify: `docs/API.md`

**Key Decisions / Notes:**

- `TrackedJob` (`reconcile/mod.rs:47-58`) gains `worker: Option<WorkerAction>`, and `TickInput` (`reconcile/mod.rs:119-132`) gains `line: LineState`, taken from `Agent.room.state`, `worker_mem` (the memory of each tracked worker, from the manager's running list) and `others` (one line that names the biggest memory users outside idlemax).
  Spec 1 adds `TickInput.lines` and `TrackedJob.admission` for its orchestrator pass in the same two structs.
  A worker job keeps the default `Admission::Guard` and is recognised by its `worker` section, because Spec 1's lines rule allows one tracked job and kills it at `shed`, while workers are many and are shed one at a time; in `plan` and `plan_starts` the worker rule is checked first.
- A `launch` or `resume` job starts unless the agent is shutting down, offline, held by the kill switch, a local pause or a coordinator `stop_all`, or `line` is not `run`; it does not use the guard's slots, RAM or user-presence verdict (D4, D13, A16).
  A tracked one never gets a guard pause or kill, and still gets every local and command hold (A24).
- A `keep`, `keep_merge` or `discard` job starts unless the agent is shutting down, whatever the line state, the kill switch or a pause says, gets no hold, and is never killed by a line or by shutdown (A10).
- A tracked worker counts against the guard's `max_jobs` and RAM budget for plain jobs, since the guard counts every reported job (`guard.rs:157-161`); Task 9 sets a worker job's `mem_mb` to `dispatcher.worker_mem_mib`.
  Spec 1 states the same for its pass.
- `shed_next(workers)` orders the tracked `launch` and `resume` workers by start time, newest first, then by priority, lowest first, then by memory, biggest first, and returns the first (D14).
  The priority is the project's, carried in the job's `priority` (A24).
- Shedding keeps going until the device is back at `run` (D14, A6).
  `AgentState` keeps a `shedding` flag: the first sample that reads `shed` sets it, and the first sample that reads `run`, or the last worker leaving, clears it.
  While it is set, every new sample that does not read `run` orders one kill, and the next one only after that worker has exited, so workers go one at a time and a reading of 92 percent after a shed at 95 still sheds.
  Spec 1's room rule has no memory of a shed, so the latch lives here.
- Under `emergency` the tick orders a kill for every tracked `launch` and `resume` worker in the same tick (D14).
- Both use a retryable `KillOrder` (`reconcile/mod.rs:64-87`) whose reason names the line, the room's reason and `others`, so the job ends `killed`, the Run becomes `shed`, the event tells the owner the biggest memory users (D14), and Task 9 relaunches the Run when there is room again.
  `others` comes from a new `ProcTable::top_memory(3, exclude)` in `runner/os.rs`, which leaves out the member pids of every worker; the agent fills it only when the line is `shed` or `emergency`.
- Plain jobs are untouched by this code, and a kill reaches only the pids Task 6 saved for that worker; the owner's sessions are never signalled (D4, D14).
- The manager lives in its own task, started in `agent::run` with `Workers::recover`.
  `execute` (`event_loop/jobs.rs:177-192`) sends `Start`, `Pause`, `Resume` and `Kill` for a job with a `worker` section over a channel and never waits for the manager, so a slow `herdr` or git call never delays sampling, the guard or a local control.
  The manager ticks every 2 s while it tracks a worker; its updates come back over a channel, pass through `AgentState::finished` (`reconcile/mod.rs:258`) and the outbox like the runner's (`event_loop/jobs.rs:132-152`), and its running list is merged into `Agent.running`, which is what `Heartbeat.running` and the lease renewal read.
- While any worker is tracked the agent samples and heartbeats every 5 s (A6, D11): the `sample` and `beat` tickers are rebuilt when the need changes, as the `reap` ticker already is (`event_loop.rs:183-188`).

**Definition of Done:**

- [x] In `reconcile` tests a `launch` job starts while the guard says `pause` for user activity and the line is `run`, is returned while the line is `hold`, and is paused by the kill switch; a plain lease in the same tick follows the guard as before.
- [x] A `discard` job starts while the line is `hold` and the kill switch is on, and an `emergency` in the same tick orders no kill for it.
- [x] With three workers and a line of `shed`, one tick orders exactly one kill, for the newest; a tie on start time picks the lowest priority, and a tie on both picks the biggest memory user.
- [x] No second kill is ordered while the first worker is still exiting or before a newer sample arrives; after a shed, a sample that reads `hold` orders one more kill and a sample that reads `run` orders none.
- [x] A line of `emergency` orders a retryable kill for all three workers in one tick and none for a plain job running beside them, and each reason holds the text of `others`.
- [x] A line of `hold` with no shed before it orders no kill.
- [x] A loop test with a `FakeHost` whose calls block shows a local pause applied to a plain job while the manager's tick is still in flight.
- [x] `docs/API.md` and the README "Safety model" section describe the worker start rule, the shed order and what an emergency does.
- [x] Verify: `cargo test -p idlemax-core reconcile && cargo test -p idlemax --bin idlemax event_loop::workers && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 8: Check a finished task and give it a verdict

**Objective:** After a worker's turn ends, gather the facts on the device, decide in pure code whether the diff stayed in scope and whether the risk is high, ask a reviewer from another provider when it is, and turn the facts into one verdict on the coordinator.
A worker saying "done" counts for nothing.

**Files:**

- Create: `crates/idlemax-core/src/worker/checks.rs`
- Create: `crates/idlemax/src/agent/worker/check.rs`
- Modify: `crates/idlemax/src/agent/worker/steps.rs`
- Modify: `crates/idlemax-core/src/worker/launch.rs`
- Modify: `crates/idlemax-core/src/worker/run.rs`
- Modify: `docs/API.md`
- Modify: `README.md`

**Key Decisions / Notes:**

- A `checking` phase follows the end of the turn; the agent reports it, then gathers `CheckFacts` in the worktree: commits since the base commit, modified tracked paths and new untracked paths as two lists (`git status --porcelain`), the changed files with added and removed lines (`git diff --numstat <base commit>..HEAD`), the subject of the last commit as the summary, the baseline exit code before and after, and the reviewer's answer.
  The check after the turn runs the project's check argv like the baseline of Task 6: through `Commands`, with the `pane_env` pairs, the tag, the throttle and `check_secs`.
  idlemax runs only the project's own check, which the owner wrote; the Task's done check is never executed by idlemax (D8).
- `scope_findings(changed, scope_paths, off_limits)` reports each changed path under an off-limits path, each changed path outside the scope paths, and "the task named no files in scope" when the list is empty (D8, A4).
- `risk_is_high(diff, review_diff_lines, core_paths, baseline_before)` is true when added plus removed lines exceed `review_diff_lines`, when a changed path lies under one of the project's `core_paths` (A4), or when the checks cannot cover the change, meaning the project has no check argv or the baseline was not recorded (D8).
- On high risk the agent runs the reviewer named in the spec, always a different provider than the author, through `Commands` with a 300 s limit, with the task title, the done check, the scope paths and the diff on standard input, and reads a JSON answer `{"verdict": "approve" or "concern", "concern": "..."}` (D8).
  `reviewer_args` in `launch.rs` returns the argv: the Claude row reuses the flags of the advisor pass (`advisor/claude.rs:30-43`) plus `--model`; the Codex row is `codex exec --sandbox read-only --ephemeral --ignore-user-config --output-schema <file> -o <file> -m <model>`.
  A diff above 200 KiB is not sent and counts as not reviewed.
  The reviewer goes through the same seam as every child process, so a test with a high-risk diff answers from `FakeCommands` and never starts a real CLI.
- `CheckFacts.review` carries the reviewer's account and model and that account's newest quota snapshots before and after the call, taken from the agent's own readings, so the spend of Task 9 counts a review for the account that paid for it (D10 item 1).
- `verdict(facts)` returns the worst finding: `rejected` when there is no commit, when a changed path is off-limits, or when the baseline passed before and fails after; `needs_you` with the exact concern when paths lie outside the scope or the task named none, when modified tracked files remain uncommitted, when the baseline failed before and still fails, when the reviewer raised a concern, or when the risk is high and no review happened; otherwise `clean` (D8).
  New untracked paths do not change the verdict; the concern of a `needs_you` and the summary of a `clean` list them by name, so the owner sees what a tool left behind (Open Question 3).
- `run::after_job` now gives a `succeeded` launch its verdict, stores the facts, the summary and the concern, and sets the end time; the pane is already closed, so the slot is free for the next task of the project (D6).
- `README.md` says what `clean` means: the diff stayed in its paths and broke no check; whether it did the task well is the owner's review.

**Definition of Done:**

- [ ] One test per rule above shows `verdict` returning `rejected`, `needs_you` with the named concern, or `clean`, and `rejected` wins over `needs_you`.
- [ ] `scope_findings` flags a path under an off-limits folder and a path outside the scope list, and passes a diff that stays inside the list.
- [ ] `risk_is_high` is true for 301 changed lines with a limit of 300, for a path under a core path, for a project with no check argv, and false for a small covered diff.
- [ ] A run with one new untracked file and nothing else wrong is `clean`, and its summary names the file; one with a modified tracked file left uncommitted is `needs_you`.
- [ ] In a temp repository with a fake host, a worker that commits one in-scope file ends with facts that give `clean`, and one that edits an off-limits file gives `rejected`.
- [ ] A high-risk diff calls the reviewer argv once through `FakeCommands`, a `concern` answer gives `needs_you`, and the facts hold the reviewer's account, model and both readings.
- [ ] `docs/API.md` describes `CheckFacts` and the verdict rules.
- [ ] Verify: `cargo test -p idlemax-core worker::checks && cargo test -p idlemax-core worker::launch && cargo test -p idlemax --bin idlemax agent::worker::check && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 9: Launch from the Plan in auto mode, one worker at a time

**Objective:** Give the `auto` mode its value and its behaviour: on each coordinator sweep, turn the next orchestrator pick of Spec 1's Plan into a Run and a leased worker job, one launch per device at a time, inside the simple account rule, the measuring week and the review cap.
The owner can see every launch and its reason.

**Files:**

- Create: `crates/idlemax-core/src/worker/pace.rs`
- Create: `crates/idlemax-core/src/worker/control.rs`
- Modify: `crates/idlemax-core/src/dispatch/mod.rs`
- Modify: `crates/idlemax-core/src/dispatch/plan.rs`
- Modify: `crates/idlemax-core/src/dispatch/orchestrator.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `crates/idlemax/src/cli/plan.rs`
- Create: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `crates/idlemax/src/coord/mod.rs`
- Modify: `crates/idlemax/src/coord/sweep.rs`
- Modify: `README.md`
- Modify: `docs/API.md`

**Key Decisions / Notes:**

- `Mode` gains `Auto` (D3, A1); it is one enum, so `dispatcher.mode = "auto"`, the route `POST /v1/dispatcher/mode` and the command `idlemax mode auto` accept it once the variant exists.
  Three places in Spec 1's code name `propose` alone and now mean "not `off`": `build` in `dispatch/plan.rs` (a device takes picks), `pass_due` in `dispatch/orchestrator.rs` (a pass may run), and `step` in `crates/idlemax/src/dispatch/mod.rs` (the base checks go on).
  `idlemax plan` prints `dispatcher: auto (workers are launched)`.
- `dispatch::launch_due` runs from `sweep_once` (`coord/sweep.rs:72`) every 15 s, after `expire`.
  It takes the `Plan` and the tasks from `gather_plan`, which reads `[dispatcher]` fresh, and reads `[workers]` from the same fresh config, so a changed setting needs no restart (A2).
  It does nothing unless the mode is `auto` and no owner stop is stored (D3).
  For each device it first relaunches the oldest `shed` run of that device that may launch, as a `resume` when a session id is stored and as a `launch` over the existing worktree when not, and otherwise takes the first pick of the device's `DevicePlan.picks` that may launch.
  A pick is launched only when its `by` is `PickedBy::Orchestrator` (A19).
- `pace::may_launch` allows a launch or a relaunch only when all of these hold (D9, D13):
  the device's `DevicePlan.held` is `None` (offline, paused, kill switch) and no paused scope holds the account (`control::is_held`, `control/mod.rs:66`);
  the room's state is `run` and its `open_slots` is at least 1;
  no run of the device is `starting`, and the last launch on the device is at least `settle_secs` old;
  the device's stored running list (`DeviceView.running`) no longer names the Run's previous job, so a relaunch never meets a worker that is still being stopped;
  the CLI is listed in `workers.clis`;
  the account is usable by `account_uses`;
  and the project has room under the cap.
  A job leased to a paused or offline device would never be delivered (`coord/api/agents.rs:181-191`), run out its lease and be launched again, which is why the pause check is here.
- The cap of D9 counts what waits for review, `pace::waiting(runs, project)` (`clean` plus `needs_you`; `rejected` does not count, A24), plus the project's runs in `starting`, `running`, `checking`, `shed` and `stopped`, against `workers.review_cap`, so work in flight cannot push a project past 3.
  `pace::waiting` is the count Spec 4 shows.
- `pace::task_open(runs, task)` decides whether a pick's task may be launched: it is false while the task has a Run in any state except `discarded`, and false for good once its last two Runs ended `rejected` or `discarded`.
  Spec 1's `discover` knows no Run, so a task stays offered until the base branch changes; without this rule a finished task would be launched again.
  A task closed for good is recorded once in an event that says the owner has to look at it.
- A shed run or a pick that may not launch is skipped and the next one is tried, so one blocked run never holds back the device.
  A refusal is recorded in an event only when its reason differs from the last one recorded for that run or pick, because the sweep runs 5,760 times a day and the events table keeps the newest 100,000 rows (`coord/store.rs:66`).
- The account rule is Spec 1's `account_uses`, called with `spent_week_pct` filled from `pace::week_spend`, so the Plan and the launch can never disagree about an account.
  `week_spend(runs, snapshots, window_start_ms)` merges the runs of one account in the current weekly window, and the reviews paid by it, into intervals that do not overlap, and sums the rise of the weekly reading once per interval, never below zero, with the newest reading standing in for a run still in flight.
  The measuring week of an account starts at its first Run and ends seven days later (`pace::measuring_week_end(first_run_ms)`); until Spec 3 lands the 10 percent cap holds in every weekly window (A7), and Spec 3 reads these two functions to fill its `measuring_left_pct`.
- When an account with live workers stops being usable, at 60 percent of its 5-hour window or at the cap, `dispatch::stop_workers` stops them with their state saved and their runs become `shed`, so they resume when the account allows it (A18).
  This is the operation Spec 3 calls for rate limits and Spec 5 for a device, and `control::plan_stop(runs, scope)` is its pure plan: a global scope gives `stopped`, an account scope gives `shed`.
- The job is built here: `name` is the pane name, `key` is `run-<id>`, `command` is the marker `["idlemax-worker", "<cli>"]`, `cwd` is unset, `devices` is the one device, `cost` is `ai`, `ai_account` is the account, `mem_mb` is `dispatcher.worker_mem_mib`, `on_user_active` is `continue` (D4), `max_attempts` is 1, `max_runtime_secs` is `workers.max_runtime_secs`, `priority` is the project's priority, and `stdin` is the brief.
  The marker is in no allowlist, so an agent that does not know the `worker` section refuses to start the job instead of running an agent CLI in the owner's folder (D6); the agent that knows it builds the real arguments with `agent_args`.
  The `WorkerSpec` takes `scope_paths` from the `Task`, `off_limits` from `Pick.avoid_paths` (the project's off-limits paths plus the owner's changed files, D5), `setup`, `check`, `core_paths` and the base branch from the `Project`, `baseline` from the project's `BaseCheck` (`crates/idlemax-core/src/dispatch/check.rs`) when it has a result, and the limits from `[workers]`.
  The reviewer is the first account of another provider that `account_uses` calls usable and whose CLI is in `workers.reviewers`; with none, the spec names no reviewer and a high-risk diff ends `needs_you`.
- At the first launch the Run stores `Pick.reason` as its `reason` (A5) and the account's newest quota snapshots (`Store::latest_quota`) as `quota_before`, and `after_job` stores them as `quota_after` each time a launch ends (D10 item 1).
  Each launch appends an event with the project, the task, the provider, the model and the reason (D3).

**Definition of Done:**

- [ ] `Config::parse("[dispatcher]\nmode = \"auto\"\n")` parses, `build` gives picks in `auto`, and `idlemax plan` shows the `auto` mode line.
- [ ] With mode `auto`, a Plan of two orchestrator picks and a usable account, the first sweep creates one Run in `starting` with the pick's reason and one job `leased` to the device, and the second pick waits until the first run is `running` and `settle_secs` have passed.
- [ ] Nothing is launched and no job row is created in mode `propose`, for a pick that is not the orchestrator's, with a CLI missing from `workers.clis`, with a room of `hold`, with `open_slots` of 0, while the device or the account is paused, or while the device is offline.
- [ ] A project with 2 runs waiting and 1 running gets no launch at a cap of 3, and the other project's pick still launches.
- [ ] A task with a `clean` Run is not launched again, one whose only Run was `discarded` is launched once more, and one with two `rejected` Runs is never launched.
- [ ] `week_spend` of two runs that overlap on one account, with weekly readings 40, 41, 43 and 44 at their four ends, is 4.0, and an account at 10.0 is not usable.
- [ ] A running worker of an account at 60 percent of its 5-hour window is stopped and its run becomes `shed`; a `shed` run with a session id is relaunched as a `resume` job before any new task starts, and not while the device still reports its previous job.
- [ ] A slot refused on ten sweeps for one reason leaves one event.
- [ ] The README describes auto mode, the fence table and what is not fenced.
- [ ] Verify: `cargo test -p idlemax-core worker::pace && cargo test -p idlemax-core worker::control && cargo test -p idlemax-core dispatch::plan && cargo test -p idlemax-core dispatch::orchestrator && cargo test -p idlemax --bin idlemax coord::dispatch && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 10: Add idlemax stop and idlemax start

**Objective:** Give the owner one command that stops every worker within seconds with its state saved and survives a reboot, and one command that continues the same sessions.
Both sit beside the existing `pause`, `resume`, `stop-all` and the `PAUSE` file, which keep working.

**Files:**

- Modify: `crates/idlemax-core/src/worker/control.rs`
- Modify: `crates/idlemax/src/coord/store/runs.rs`
- Modify: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `crates/idlemax/src/coord/api/runs.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Create: `crates/idlemax/src/cli/workers.rs`
- Modify: `crates/idlemax/src/cli.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/client.rs`
- Modify: `crates/idlemax/src/cli/home/render.rs`
- Modify: `skills/idlemax/SKILL.md`
- Create: `crates/idlemax/tests/workers.rs`
- Modify: `README.md`
- Modify: `docs/API.md`

**Key Decisions / Notes:**

- Stop and start are two coordinator routes (A3).
  `POST /v1/workers/stop` takes `{"scope": ..., "reason": ...}` with the existing `ControlScope` (`types.rs:25-30`): `global` is the owner's stop, `account` is the operation for Spec 3, and a `job` or `device` scope is 400 until Spec 5 adds the device scope.
  A global stop writes the `worker_stop` row, moves every run in `starting`, `running` or `checking` to `stopped` and marks its job `killed`, in one transaction (D11).
  The agents hear it as `kill_job` on their next heartbeat, which Task 7 made at most 5 s away, and stop each worker with its state saved.
- The stop works whatever the line state, the guard or a pause says, because it is one write in the store (A10).
- Saved per worker is what the Run already holds: project, task, branch, worktree, provider, account, model and session id (D11).
  The stop lives in SQLite, so it survives a reboot, and `launch_due` starts nothing while the row exists.
- `POST /v1/workers/start` deletes the row and moves every `stopped` run to `shed` with `control::plan_start`, which is the queue `launch_due` already drains one at a time after it checked the line state and the account rule again (D11).
  Nothing else ever moves a run out of `stopped`, so idlemax never continues an owner's stop by itself.
- Beside the existing controls: `pause` and the `PAUSE` file freeze workers in place like jobs and start none, `resume` only lifts a pause, and `stop-all` or `idlemax local stop-all` stop the workers in scope with their state saved (Task 3 puts their runs in `stopped`), after which `idlemax start` continues them once the pause that `stop-all` left is lifted.
  The start reply names every pause still in force, with the `idlemax resume` command that lifts it; Task 9 launches nothing into a paused scope.
- `idlemax stop [--reason TEXT]` and `idlemax start` are new commands in `cli/commands.rs`; the existing `idlemax resume` keeps its meaning.
  Both print what changed and the next command, take `--json`, and report "nothing to do" as a success.
- `COMMANDS` in `cli/home/render.rs:18` gains `idlemax stop` and `idlemax start`, and `skills/idlemax/SKILL.md` is regenerated with `cargo run -p idlemax -- skill > skills/idlemax/SKILL.md`, which needs no state dir.

**Definition of Done:**

- [ ] A global stop with two running workers leaves both runs `stopped`, both jobs `killed` and the stop row stored; a store opened again on the same file still holds the row and `launch_due` launches nothing.
- [ ] A start removes the row and moves both runs to `shed`; a run stopped by `stop-all` stays `stopped` until a start.
- [ ] An account stop moves only that account's runs, to `shed`, and stores no row; a `device` scope answers 400.
- [ ] A global stop sent while a global pause is in force still stops both runs.
- [ ] Against a coordinator in a temp home with no runs, `idlemax stop --json` and `idlemax start --json` exit 0 and report zero workers, and the test asserts the home is not the real one as `tests/e2e.rs:75` does.
- [ ] The README "Controls" section and `docs/API.md` "Control semantics" say how `stop` and `start` differ from `pause`, `resume`, `stop-all` and `PAUSE`.
- [ ] Verify: `cargo test -p idlemax-core worker::control && cargo test -p idlemax --bin idlemax api::runs && cargo test -p idlemax --test workers && cargo test -p idlemax --test skill && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 11: Add idlemax review with keep, merge and discard

**Objective:** Let the owner see every finished task per project and take or drop each one: keep the branch, keep and merge it into the current branch when that is safe, or discard branch and folder after one confirmation.
The device's agent does the work on disk, so the same commands hold for any device, and no line or pause holds them.

**Files:**

- Create: `crates/idlemax/src/agent/worker/review.rs`
- Modify: `crates/idlemax/src/agent/worker/mod.rs`
- Modify: `crates/idlemax-core/src/worker/run.rs`
- Modify: `crates/idlemax/src/coord/api/runs.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/cli/workers.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/client.rs`
- Modify: `crates/idlemax/src/cli/home/render.rs`
- Modify: `skills/idlemax/SKILL.md`
- Modify: `crates/idlemax/tests/workers.rs`
- Modify: `README.md`
- Modify: `docs/API.md`

**Key Decisions / Notes:**

- `idlemax review` lists the runs in `clean`, `needs_you` and `rejected` per project with the verdict and its concern, the one-line summary, the diff size and the branch, and below them how many runs are stopped or waiting to restart (D16).
- `POST /v1/runs/{id}/keep` with `{"merge": bool}` and `POST /v1/runs/{id}/discard` create one worker job with the action `keep`, `keep_merge` or `discard`, leased to the run's device; they answer 404 for an unknown run, with a hint that names `idlemax review`, and 409 for a run that has a live job or is already `kept` or `discarded`.
  Keep is allowed for the three verdict states, and discard also for `stopped` and `shed`.
  The command waits for that job with the existing wait code (`cli/job_wait.rs:46`) and prints its result or the reason it was refused.
- These jobs are owner actions and are never held (A10).
  `assign_jobs` (`coord/api/agents.rs:147-213`) sends a leased `keep`, `keep_merge` or `discard` job to its device even when the device, the account or everything is paused, and Task 7 starts it whatever the line state says.
- `keep` removes the worktree folder with git's safe removal and leaves the branch; the run becomes `kept` (D16).
- `keep_merge` works in the owner's checkout: it refuses when `git status --porcelain` is not empty or `HEAD` is detached, tests the merge with `git merge-tree --write-tree HEAD <branch>` without touching the tree and refuses on a conflict, then runs `git merge --no-edit <branch>` with a limit of 120 s, and only then removes the folder (D16).
  When the merge fails or passes its limit after it started, for example in a hook or at a commit that must be signed, the agent runs `git merge --abort`, confirms that `git status --porcelain` is empty again, and then reports.
  Each refusal says why and what to do, and leaves the run and the tree as they were.
- `discard` removes the folder with git's safe removal and then deletes the branch; when git refuses because files are left, nothing is deleted and the message names the folder and the files (D16).
  The command asks one confirmation that names the branch and the diff size, `--yes` skips it, and without a terminal and without `--yes` it is a usage error.
- `run::after_job` maps a `succeeded` keep job to `kept` and a `succeeded` discard job to `discarded`; a failed or killed one leaves the state and the verdict unchanged.
  Each keep or discard frees room under the cap of Task 9 for the next task (D9).
- `COMMANDS` in `cli/home/render.rs:18` gains `idlemax review`, and the skill is regenerated.

**Definition of Done:**

- [ ] In a temp repository, keep removes the folder and leaves the branch, and discard removes both; discard on a worktree with an uncommitted file deletes nothing and reports git's refusal.
- [ ] Merge refuses a dirty owner checkout and a conflicting branch with the reason, leaving `git status` and `HEAD` unchanged, and merges a clean non-conflicting branch into the current branch.
- [ ] A merge whose `git merge` call fails after it started leaves `git status` empty and `HEAD` unchanged.
- [ ] The keep route answers 409 for a run that is `running` and 404 for an unknown id, and a succeeded keep job leaves the run `kept` with its verdict still stored.
- [ ] A leased `discard` job is in the heartbeat reply of a device under a global pause, and a leased `launch` job is not.
- [ ] `idlemax review --json` against an empty coordinator prints an empty list, and `idlemax review discard 5` without `--yes` and without a terminal exits 2 with a message that names `--yes`.
- [ ] The README documents the three commands, and `skills/idlemax/SKILL.md` lists `idlemax review`.
- [ ] Verify: `cargo test -p idlemax --bin idlemax agent::worker::review && cargo test -p idlemax --bin idlemax api::runs && cargo test -p idlemax --bin idlemax worker_tests && cargo test -p idlemax --test workers && cargo test -p idlemax --test skill && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 12: Needs the owner's OK: prove each CLI and the fences on the Mac, then switch on auto

**Objective:** Run the written check of `docs/WORKER-PROOF.md` for Claude, and for Codex if the owner wants it, in a throwaway setup, record the results, and only then let that CLI run workers on the real machine.
This task starts real agent sessions, spends a small amount of real quota and changes the owner's real settings, so it runs only with the owner's OK.

**Files:**

- Modify: `docs/WORKER-PROOF.md`
- Modify: `docs/ROADMAP.md`

**Key Decisions / Notes:**

- The owner's OK for this task covers the soft fences: the answer on the worker fence is pending (A13), the README names what is not fenced, and `docs/WORKER-PROOF.md` records the date of the OK.
  No server is touched; servers stay in `propose` until Spec 5 and that answer exist (A15).
- The setup is throwaway: a temp `IDLEMAX_HOME` asserted before every command as `AGENTS.md` shows, non-default ports, a temp git repository with one tracked file and a local bare remote, a project that points at it, and a task that adds one line to a file.
  The real `HOME` stays, because the agent CLI needs its login; no `launchctl`, no service install and no statusline or hook install is run, and the real `~/.idlemax` is never read or written.
- The first step lists, in a plain new Herdr pane, the names `env_problems` would report.
  A name the Herdr server passes on is blanked by the launch; a name a shell start-up file exports has to be guarded there by the owner before any CLI is tried.
- Per CLI the check fills every row of its table in `docs/WORKER-PROOF.md` in the order written there (D11, D22).
  The memory row gives the figure for `dispatcher.worker_mem_mib`, which Spec 1 ships as 512 until a worker is measured.
  The token row and the hooks row give the facts for Open Question 4, and the background-class row those for Open Question 5.
- A CLI enters `workers.clis` and `agent.command_allowlist` of the real config only when every row of its table passed, and a reviewer CLI enters `workers.reviewers` only when its review row passed; a row that fails is recorded with what was seen, and that CLI stays off.
- Switching the real dispatcher mode to `auto` with `idlemax mode auto` is the last step and is the owner's own action.
- The temp directory, the proof workspace and every daemon started for the check are removed at the end.

**Definition of Done:**

- [ ] `docs/WORKER-PROOF.md` holds the date of the owner's OK and a dated result for every row of the Claude table, and of each other CLI and reviewer the owner chose.
- [ ] On the Mac in `auto` mode one real task reaches a verdict, `idlemax stop` frees every worker within seconds, `idlemax start` continues the same sessions, and no branch or commit of a worker exists on any remote.
- [ ] Roadmap item 34 in `docs/ROADMAP.md` records that result.
- [ ] Verify: `grep -n "Result:" docs/WORKER-PROOF.md`; the task adds no code and no test, so `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass as before.

## Open Questions

1. **How strong is the worker fence (A13).**
   D22 says a worker starts with no push access and no deploy or database credentials, and a worker runs as the owner's own user.
   Herdr can only add variables to a pane (`herdr pane split` and `herdr tab create` take `--env KEY=VALUE` and nothing that clears), so ssh keys, the `gh` login, cloud CLI logins, the keychain, the idlemax token file and `coord.db` stay readable to a worker that goes looking, and a git-level push block can be undone on purpose.
   Pointing `IDLEMAX_HOME` at an empty folder in the pane would hide the state dir from the `idlemax` command, but the Claude statusline capture is written under it (`crates/idlemax/src/agent/quota_probe.rs:135`), so a worker's session would stop refreshing the quota reading.
   Recommended: the owner chooses between a dedicated OS user and an OS sandbox profile before any server runs in `auto` (A15); on the Mac this plan ships the soft fences, with the gap in the README and the owner's OK in Task 12.
2. **The pane name and the retry suffix extend D15.**
   D15 fixes the pane name `im-<project>-<task>`, the branch `idlemax/<task>` and the folder `$IDLEMAX_HOME/worktrees/<project>/<task>`.
   Two runs of one task need two pane names, and a task tried again must not reuse the old branch (D9), so Task 4 adds the run id to the pane name and `-r<run id>` to the branch and folder of a second try.
   Recommended: record both in D15 of `docs/DISPATCHER.md`.
3. **Files a tool leaves untracked.**
   A worker's own tools can write untracked files that no ignore rule covers, and `git worktree remove` without force then refuses, so keep and discard fail until the owner cleans the folder by hand (D16 forbids force).
   Task 8 already keeps such files out of the verdict and lists them by name.
   Recommended: Spec 1 adds `disposable_paths` to `ProjectSettings`, and keep and discard delete exactly those paths by name before git's safe removal.
4. **Does a Claude worker load the owner's user settings.**
   D10 item 9 says a worker loads the project's own instructions and not the owner's personal MCP servers; it does not speak of personal hooks, plugins and global instructions, which load with the user settings and can add tens of kilobytes to every session or keep a turn from ending.
   Herdr's integration and the statusline capture also live in the user settings, and `claude --bare` would switch off the subscription login.
   Recommended: decide from the token row and the hooks row of Task 12; if the cost is high, start workers with `--setting-sources project,local` plus a `--settings` file that holds only the Herdr hook and the statusline, after a proof row shows that Herdr still reports status.
5. **The runner's throttle on a process in a pane (A16).**
   A16 says workers keep the runner's OS throttles, and the runner applies them by wrapping the argv it starts (`crates/idlemax-core/src/job_run/wrap.rs:43-68`), which is not possible for an agent that Herdr starts.
   `taskpolicy -b -p <pid>` exists on the Mac and sets the background class on a running process; the memory limit of the wrapper has no such form, and the lines of D14 stand in for it.
   Whether the background class slows an agent's network calls is not measured.
   Recommended: keep the rule of Tasks 5 and 6, and lift it for the agent process itself if the row in `docs/WORKER-PROOF.md` shows the sample task more than twice as slow, keeping it for setup and check.

## Review Disposition

1. fixed
2. fixed
3. fixed
4. fixed
5. fixed
6. fixed
7. fixed
8. fixed
9. fixed
10. fixed
11. fixed
12. fixed
13. fixed
14. fixed
15. fixed
16. fixed
17. fixed
18. fixed
19. fixed
20. fixed
21. fixed
22. not applied: A12 says the worker marker is the existing `IDLEMAX_JOB_ID` and that no second marker is added; `pane_env` already sets it (`crates/idlemax/src/agent/runner/launch.rs:17`), and Task 5 now names it as the marker.
23. fixed
24. fixed
25. fixed
26. fixed
27. fixed
28. fixed differently: A13 leaves the fence strength pending and tells this plan to build the soft fences, so Task 12 does not wait for an amended D22; it runs only with the owner's OK for the soft fences, records the date, and keeps servers out (A15).
29. fixed differently: the format caps a plan at 12 tasks, so no task was added; the pure tick rules of old Task 6 and all of old Task 9 became Task 7, the manager stayed alone in Task 6, and the `[workers]` settings and the brief moved out of the launch task into Tasks 1 and 5.
30. fixed
31. fixed
32. fixed
33. fixed
34. fixed
35. fixed
36. fixed
37. not applied: the headless host belongs to Spec 5 (spec map D19; `docs/plans/2026-11-11-dispatcher-5-servers.md:496-545` builds `HeadlessHost`), so building a second real host here would build what another spec owns; the `Commands` seam of Task 4 gives every other test a fake, and an end-to-end test over a stub CLI comes with that host.
38. fixed
