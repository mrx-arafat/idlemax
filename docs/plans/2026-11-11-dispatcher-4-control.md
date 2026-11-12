# Dispatcher 4: Control and Visibility Implementation Plan

Created: 2026-11-11
Agent: Claude Code
Status: IN_PROGRESS
Approved: Yes
Iterations: 1
Worktree: No
Type: Feature

## Summary

**Goal:** The owner sees every worker with a state label and switches the dispatcher from the dashboard, the TUI or an agent they instruct through `idlemax mcp`, is interrupted for three things only, and a job or worker that idlemax launched is refused every on-switch even when it has read the token.

## Out of Scope

- The CLI commands `plan`, `project`, `mode`, `start`, `stop`, `review` and `device`, and the routes behind them (Specs 1, 2 and 5; the mode route and command are Spec 1's by A1).
- Keep and discard in the dashboard or the TUI: the review list is read-only here and both actions stay in `idlemax review` (Spec 2).
- The re-read of `config.toml` when its mtime changes (A2): Specs 1, 2 and 3 each build it for their own keys, and this spec only writes the file through the config editor.
- An installer that edits each agent CLI's settings for the MCP server: each of the four CLIs has its own `mcp add` command, so the owner runs one line per CLI.
- Changing a limit of another device: `POST /v1/limits` edits the `config.toml` of the machine the coordinator runs on, and A23 has every other device's limits changed on that device and shown read-only here.
- Keeping a launched job or worker from queueing plain jobs, schedules or advice: those routes keep today's token-only rule.
- An OS sandbox or a separate OS user for workers: the strength of that fence is the owner's pending answer (A13, Open Question 1).
- The credential per device that reaches only the `/v1/agent/*` routes (A15, Spec 5).
- An MCP tool for the mode, limits or rule proposals: D12 fixes the tool list, and an agent the owner instructs changes those with `idlemax mode` and `idlemax config set` from its own shell.

## Approach

**Chosen:** One read model, `DispatcherView` from `GET /v1/dispatcher`, feeds the dashboard, the TUI, `idlemax status` and the MCP status tool; one access rule in `idlemax_core::control::caller` is checked in the CLI and again on the coordinator and the agent local API by looking at the calling process; notices are decided by pure functions on the coordinator and reach every agent through the heartbeat reply, where the inbox entry is always written and the desktop notification follows `agent.notify` (A22).
**Why:** Labels, summaries and the "needs you" list are computed once in `idlemax-core`, so the surfaces cannot drift, and an on-switch is refused even for a launched process that reads the token file and calls the API by hand; the cost is one process lookup per owner-only call and two store migrations.

**The lock: options weighed (D12).**
A worker runs as the owner's OS user, so it can read `$IDLEMAX_HOME/token`.

| Option | What it stops | Why it is not enough alone |
|---|---|---|
| A marker variable in the launched process's environment that the CLI checks | a job or worker that runs `idlemax start` | the process can skip the CLI and call the HTTP API with the token |
| A second, owner-only token file | nothing more | any file the owner can read, the worker can read |
| The owner token in the OS keychain | reads by other programs | both crates forbid `unsafe` and the binary is unsigned, so the keychain cannot tell idlemax from a worker's shell, and a headless server has none |
| A passphrase the owner types | API calls without it | the owner types it for every switch, and an agent session needs it stored in a file |
| The server looks at the calling process | API calls from any process whose ancestors include a launched job or worker | it cannot see a process on another machine, or one that cut itself off from its ancestors |

Chosen: the first and the last together.
The marker is the existing `IDLEMAX_JOB_ID` and nothing else (A12): the runner sets it on every plain job, and Spec 2's `pane_env` passes it into every worker's pane.
The CLI and `idlemax mcp` refuse an owner-only action when their own environment carries the marker, which gives a clear error with no network call.
The coordinator and the agent local API then find the process behind each owner-only request, walk its ancestors, and refuse when the caller or any ancestor carries the marker of this idlemax home.
A worker gets no idlemax token at all (A14: `pane_env` blanks `IDLEMAX_TOKEN`), so through the CLI and `idlemax mcp` it reaches no route; the lock is the second layer, for a worker that reads the token file by its path and for plain jobs, which do read the token file today.

**What the lock does not stop.**
Task 4 writes this list into `README.md`, and this spec does not close it.

- A write to `config.toml` in the state dir: every `[dispatcher]`, `[workers]` and quota key, the mode included, applies at once, because each daemon re-reads those keys when the file's mtime changes (A2); every other key applies at the next reload or start.
- A write to `rules.md`, the store or the token file, or deleting the `PAUSE` file: it is the same OS user.
- Detaching: one line such as `( env -u IDLEMAX_JOB_ID curl ... & )` leaves a caller with a clean environment under pid 1.
- A call to a coordinator on another machine: a caller that is not on the coordinator's host is judged by the token alone, until Spec 5's credential per device (A15).
- Acting through the owner's own session: a shell start-up file, a git hook or a service file that the owner's session runs later, or a script that drives the owner's browser.
- Replacing the `idlemax` binary.

So the lock stops accidents and a launched process that tries the documented switches, and D12's "can never" needs a boundary the OS enforces at launch, which waits for the owner's answer (A13, Open Question 1).

## Context for Implementer

Specs 1, 2 and 3 land before this one (D21), and Spec 5 after it.
This plan uses their code by the paths and symbols their specs give; read the landed code for the exact signature before writing a line that uses it.

| What this plan uses | Where it lives |
|---|---|
| `Mode`, `LineState`, `RoomDecision`, `Project`, `ProjectView`, `Task`, `Plan`, `DevicePlan`, `Pick` | `crates/idlemax-core/src/dispatch/` (Spec 1) |
| `gather_plan`, `step` | `crates/idlemax/src/dispatch/mod.rs` (Spec 1) |
| `Store::list_projects`, the routes `POST /v1/projects`, `POST /v1/projects/{id}/on`, `/off` and `/settings` | `crates/idlemax/src/coord/store/projects.rs`, `crates/idlemax/src/coord/api/projects.rs` (Spec 1) |
| `GET /v1/plan`, `POST /v1/plan/refresh`, `ingest_pass` | `crates/idlemax/src/coord/api/plan.rs` (Spec 1) |
| `POST /v1/dispatcher/mode`, `idlemax mode` with `off`, `propose` or `auto`, the key `dispatcher.mode` | Spec 1, with `auto` added by Spec 2 (A1) |
| `schema`, `validate`, `PassResult`, `PASS_JOB_NAME` | `crates/idlemax-core/src/dispatch/orchestrator.rs` (Spec 1) |
| `summary`, `prompt` | `crates/idlemax-core/src/dispatch/prompt.rs` (Spec 1) |
| `Paths::rules_file()`, `rules_file::read` and its 16 KiB clip | `crates/idlemax-core/src/paths.rs`, `crates/idlemax/src/dispatch/rules_file.rs` (Spec 1) |
| the client calls of the dispatcher commands | `crates/idlemax/src/cli/client/dispatch.rs` (Spec 1) |
| `Run` with its `reason` (A5), `RunState`, `Verdict` | `crates/idlemax-core/src/worker/run.rs` (Spec 2) |
| `JobSpec.worker`, `pane_env` | `crates/idlemax-core/src/types/job.rs`, `crates/idlemax-core/src/worker/launch.rs` (Spec 2) |
| `pace::may_launch`, the key `workers.review_cap` | `crates/idlemax-core/src/worker/pace.rs`, the `[workers]` section (Spec 2) |
| the Run rows, `GET /v1/runs`, `POST /v1/workers/stop`, `POST /v1/workers/start`, `POST /v1/runs/{id}/keep`, `POST /v1/runs/{id}/discard` | `crates/idlemax/src/coord/store/runs.rs`, `crates/idlemax/src/coord/api/runs.rs` (Spec 2) |
| `FakeHost` | `crates/idlemax/src/agent/worker/host.rs` (Spec 2) |
| `Budget`, `BudgetBasis`, `AccountDay` | `crates/idlemax-core/src/quota/budget.rs`, `crates/idlemax-core/src/quota/owner_use.rs` (Spec 3) |
| `budgets`, the `quota_days` rows, the key `quota.owner_reserve_pct` | `crates/idlemax/src/coord/api/budget.rs`, `crates/idlemax/src/coord/store/budget.rs`, `[quota]` (Spec 3) |
| `DeviceView.enabled`, `POST /v1/devices/{id}/on` and `/off` | Spec 5, which lands after this plan |

Every task that adds or classifies a route uses this access table, and no task may put a route in a weaker class.

| Access | Calls |
|---|---|
| `Read` | every `GET` |
| `Stop` | `pause`, `stop_all` and `cancel` on `POST /v1/control` and `POST /v1/local/control`; `POST /v1/workers/stop`; `POST /v1/projects/{id}/off`; `POST /v1/dispatcher/mode` toward `off`; `POST /v1/devices/{id}/off` |
| `Owner` | `resume` on both control routes; `POST /v1/local/reload`; `POST /v1/workers/start`; `POST /v1/dispatcher/mode` toward `propose` or `auto`; `POST /v1/projects`; `POST /v1/projects/{id}/on`; `POST /v1/projects/{id}/settings`; `POST /v1/plan/refresh`; `POST /v1/runs/{id}/keep`; `POST /v1/runs/{id}/discard`; `POST /v1/limits`; `POST /v1/proposals/{id}/approve` and `/reject`; `POST /v1/devices/{id}/on` |
| token only, as today | the writes under `/v1/jobs`, `/v1/schedules` and `/v1/advice`, and the `/v1/agent/*` routes |

The two device rows are wired by Spec 5 when it adds those routes; they are listed here so that it has the class.
`Stop` keeps today's rule that any holder of the token may stop work; it is not a way for a worker to stop idlemax, because a worker holds no token (A14).
The access check never looks at the line state, the guard or a pause, so no owner action is held by them (A10).

Every manual check and every test runs in a throwaway home with the assert of `AGENTS.md`, on spare ports.
No dev run configures an AI account, sends a prompt to a real agent CLI, starts a worker, or touches `launchctl` or `systemctl`.
The kill switch `$IDLEMAX_HOME/PAUSE` and the guard are not changed by any task.

## File Structure

- `crates/idlemax-core/src/dispatch/label.rs` (create) - pure: run state to label and one-line summary.
- `crates/idlemax-core/src/dispatch/view.rs` (create) - pure: `dispatcher_view(input, now_ms) -> DispatcherView`, the wire type every surface renders.
- `crates/idlemax-core/src/dispatch/proposal.rs` (create) - pure: the `Proposal` type, admission of a proposed line, the append to `rules.md` text.
- `crates/idlemax-core/src/control/caller.rs` (create) - pure: the marker name, `Access`, `Caller`, `classify`, `permit`, and the parsers that find a peer process in `lsof` and `/proc` text.
- `crates/idlemax-core/src/config/limits.rs` (create) - pure: the list of owner limits and the edit that refuses any other key.
- `crates/idlemax-core/src/notify/notice.rs` (create) - pure: the three notice kinds, what is worth a notice, the 10 minute batch, the daily summary, the inbox note text.
- `crates/idlemax-core/src/mcp.rs` (create) - pure: MCP messages in and out, the six tools.
- `crates/idlemax/src/files.rs` (moved from `crates/idlemax/src/cli/files.rs`) - adapter: atomic write and backup, with a typed error.
- `crates/idlemax/src/coord/api/dispatcher_view.rs` (create) - adapter: `GET /v1/dispatcher`.
- `crates/idlemax/src/coord/api/caller.rs` (create) - adapter: finds the calling process and applies `permit`.
- `crates/idlemax/src/coord/api/limits.rs` (create) - adapter: `GET` and `POST /v1/limits`.
- `crates/idlemax/src/coord/api/proposals.rs` (create) - adapter: list, approve, reject.
- `crates/idlemax/src/coord/store/proposals.rs` (create) - adapter: the `rule_proposals` table.
- `crates/idlemax/src/coord/store/notices.rs` (create) - adapter: the `notices` and `notice_items` tables and the read of finished plain jobs.
- `crates/idlemax/src/coord/notices.rs` (create) - adapter: the sweep step that creates notices.
- `crates/idlemax/src/agent/notices.rs` (create) - adapter: the cursor file, the inbox note and the desktop notification for each notice.
- `crates/idlemax/src/cli/out/dispatcher_view.rs` (create) - delivery: the TOON node of the dispatcher block in `idlemax status`.
- `crates/idlemax/src/cli/mcp.rs` (create) - delivery: the stdio loop of `idlemax mcp`.
- `crates/idlemax/src/tui/dispatcher.rs` (create) - delivery: state and keys of the Dispatch tab.
- `crates/idlemax/src/tui/dispatcher_view.rs` (create) - delivery: drawing of the Dispatch tab.
- `crates/idlemax/tests/fixtures/dispatcher_demo.sql` (create) - seed rows for the route test, the browser scenarios and the screenshots.
- `crates/idlemax/tests/worker_lock.rs` (create) - integration test of the lock.
- `crates/idlemax/tests/mcp.rs` (create) - integration test of `idlemax mcp`.
- `scripts/test-mcp-clients.sh` (create) - adds and lists the server in each installed agent CLI, in a throwaway home.

## Assumptions

- Specs 1, 2 and 3 are on `main` before Task 1 starts (D21) - all tasks depend on this.
- Spec 1 provides `POST /v1/dispatcher/mode` and `idlemax mode` (A1), and every project, start and stop switch is a coordinator route with on and off as separate calls (A3) - Tasks 4, 9, 10 and 11 depend on this.
- Each daemon re-reads the `[dispatcher]`, `[workers]` and quota keys it uses when the mtime of `config.toml` changes (A2) - Task 5 depends on this.
- Spec 2's `Run` carries `reason`, copied from the Plan slot at launch (A5) - Tasks 1, 9 and 10 depend on this.
- Every worker's pane carries `IDLEMAX_JOB_ID` (A12) and an empty `IDLEMAX_TOKEN` (A14) - Tasks 3, 4, 11 and 12 depend on this.
- The owner limits are `config.toml` keys that `list_settings` returns, because Specs 1 and 2 add `dispatcher` and `workers` to its sections and `quota` is one already - Task 5 depends on this.
- The orchestrator pass output of Spec 1 is a typed value whose `schema` and `validate` can take one more optional field - Task 6 depends on this.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| The process lookup fails or is slow, and the owner cannot switch the dispatcher on | Low | High | The lookup has a 2 second limit, `Stop` access never runs it, the 403 text names the lookup that failed, and a test covers "lookup fails, stop still works"; the `lsof` call took 0.02 s on the owner's Mac with about 780 processes. |
| The owner reads the lock as a fence | Medium | High | `README.md` and `docs/API.md` carry the list "What the lock does not stop" word for word, and Open Question 1 stays open until the owner answers A13. |
| The repository's own tests fail when a job or worker of idlemax runs them | Medium | Medium | The lock counts a marked process only for its own home, the runner pins `IDLEMAX_HOME` on every job, the test helpers drop the marker from the binaries they start, and `worker_lock` proves the other-home case. |
| This spec and another add the same migration number | Medium | Medium | Tasks 6 and 7 expect V11 and V12, check `MIGRATIONS` on `main` at the time of the commit and take the next free number. |
| A notice is delivered twice after a crash between the note and the cursor | Low | Low | The inbox note is named by the notice id, so the second write replaces the first; only the desktop notification can repeat once. |
| The seventh tab pushes the TUI tab bar past 80 columns | Medium | Medium | A unit test asserts that `tab_titles` for the seven titles fits 80 cells, and every snapshot is regenerated and reviewed in Task 10. |

## Goal Verification

### Truths

1. The label and the summary of one worker are the same text in the dashboard, the TUI, `idlemax status --json` and the MCP `status` tool at the same moment.
2. One task finishing in `auto` mode gives each device exactly one inbox entry for it, and one desktop notification only where `agent.notify` is true; a worker start, a hold, a shed, a stopped worker job and a move to another provider in the same hour produce none, and a plain job that finishes in the same 10 minutes is a line of the same message.
3. From a process that holds the token and has a launched job or worker of this home among its ancestors, start, a mode change to `auto`, project on, keep with merge, a limit change and `resume` are refused through the CLI, through `idlemax mcp` and through a direct HTTP call, while the same calls from the owner's terminal succeed.

## E2E Test Scenarios

All scenarios run against a coordinator from the checkout build in a throwaway home on spare ports, with no agent running, so no seeded job can reach a runner.
The coordinator is started once to create the store and stopped, `crates/idlemax/tests/fixtures/dispatcher_demo.sql` is loaded with `sqlite3` into the file `Paths::db_file()` names, and the coordinator is started again.
The fixture holds one device, two projects (one on, one off), runs in `running`, `checking`, `clean` and `needs_you`, one Task with a "needs you" reason, and one open rule proposal.
No AI account is configured, so no Budget exists in these scenarios.

### TS-001: Switch the dispatcher and stop it
**Priority:** Critical
**Preconditions:** The fixture is loaded and `idlemax mode off` was run in the throwaway home.
**Mapped Tasks:** Task 2, Task 4, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Open the dashboard, paste the throwaway token, click Connect | The Dispatcher card shows the mode `off` selected and two worker rows with the labels `working` and `checking` |
| 2 | Click `Propose` in the mode switch | A toast names the mode `propose`, and "Would launch next" shows no row and the line that says why (the seeded device is offline) |
| 3 | Click `Auto`, then confirm the dialog | The switch shows `auto` selected and a toast names it; no new worker row appears |
| 4 | Click `Stop workers`, then confirm | Within 5 s both worker rows show `stopped by you` and a `Start` button is shown |
| 5 | Click `Start`, then confirm | Both rows show `back in queue` |
| 6 | Click `Off` | The switch shows `off` selected with no dialog |

### TS-002: A worker row explains itself
**Priority:** High
**Preconditions:** The fixture is loaded.
**Mapped Tasks:** Task 1, Task 2, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Read the Workers table | Each row shows the label, the one-line summary, the provider and the model |
| 2 | Click the row's "why" line | The reason stored on the Run opens under the row |
| 3 | Read the "Waiting for review" card | Two rows with the labels `needs you` and `ready for review`, each with its branch, no keep or discard button, and the text `idlemax review` |

### TS-003: Switch a project on and off
**Priority:** High
**Preconditions:** The fixture is loaded.
**Mapped Tasks:** Task 4, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Click the toggle of the project that is off, then confirm | The toggle shows on and a toast names the project |
| 2 | Click the same toggle again | It shows off with no dialog |
| 3 | Read the device area | A toggle for each device whose `enabled` is set, showing its switch (`mac` on, as the fixture stores it); before Spec 5 sets `enabled` no device toggle is shown |

### TS-004: Change a limit
**Priority:** High
**Preconditions:** The Limits card lists the owner limits.
**Mapped Tasks:** Task 5, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Type `88` into `dispatcher.hold_mem_used_pct` and click Save | The row shows 88 and "applied now", and `config.toml` in the throwaway home holds the key |
| 2 | Type `banana` into the same input and click Save | A red toast shows the reason from the coordinator and the row still shows 88 |
| 3 | Click the row's "default" button | The row shows 90 again and the key has left `config.toml` |

### TS-005: Approve a rule proposal
**Priority:** Medium
**Preconditions:** The fixture is loaded.
**Mapped Tasks:** Task 6, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Read the "Rule proposals" card | The proposed line, its reason and the run it came from are shown |
| 2 | Click Approve, then confirm | A toast says the line was added to `rules.md`, the row leaves the open list, and `rules.md` in the throwaway home ends with the line |
| 3 | Reload the page | The open list is empty |

### TS-006: The needs you list and the empty budget
**Priority:** Medium
**Preconditions:** The fixture is loaded.
**Mapped Tasks:** Task 1, Task 9

| Step | Action | Expected Result |
|------|--------|-----------------|
| 1 | Read the "Needs you" card | One row with the project, the task and the reason |
| 2 | Read the Budget card | One line that says no AI account is configured |

## Progress Tracking

- [x] Task 1: Add the state labels and the dispatcher view to the core
- [x] Task 2: Serve the view and show it in `idlemax status`
- [x] Task 3: Decide who is calling
- [x] Task 4: Refuse owner-only calls from a launched process
- [x] Task 5: Change limits through the API
- [x] Task 6: Store, list and approve rule proposals
- [x] Task 7: Decide and store the three kinds of notice
- [x] Task 8: Deliver a notice to the desktop and the inbox
- [ ] Task 9: Add the dispatcher controls and views to the dashboard
- [x] Task 10: Add the Dispatch tab to the TUI
- [ ] Task 11: Add `idlemax mcp`
- [ ] Task 12: Needs the owner's OK: switch control and visibility on for real

> Source of truth for completion.
> `spec-implement` toggles `[ ]` to `[x]` here.

## Implementation Tasks

### Task 1: Add the state labels and the dispatcher view to the core

**Objective:** Create the one mapping from Spec 2's `RunState` to a label and a one-line summary, and the pure function that builds everything the owner looks at from the Plan, the Runs, the Projects and the Budget.
Every surface in the later tasks renders this output and computes nothing itself.

**Files:**

- Create: `crates/idlemax-core/src/dispatch/label.rs`
- Create: `crates/idlemax-core/src/dispatch/view.rs`
- Modify: `crates/idlemax-core/src/dispatch/mod.rs`
- Modify: `crates/idlemax-core/src/config/settings.rs`
- Test: `crates/idlemax-core/src/dispatch/label.rs`, `crates/idlemax-core/src/dispatch/view.rs` (unit tests at the end of each file)

**Key Decisions / Notes:**

- D17, the state labels (a shared name this spec provides).
  `run_label(state, frozen) -> &'static str` and `run_summary(&RunLine) -> String` take Spec 2's `RunState` (`crates/idlemax-core/src/worker/run.rs`), and the text is fixed here:

  | State | Label | Summary |
  |---|---|---|
  | `starting` | `starting` | `setting up <task>` |
  | `running` | `working` | `<task> on <provider> <model>` |
  | `checking` | `checking` | `idlemax is checking <task>` |
  | `clean` | `ready for review` | `<task>: checks passed` |
  | `needs_you` | `needs you` | `<task>: <concern>` |
  | `rejected` | `rejected` | `<task>: <concern>, branch kept` |
  | `stopped` | `stopped by you` | `<task>: state saved, continues after you start it again` |
  | `shed` | `back in queue` | `<task>: state saved, restarts when memory and quota allow` |
  | `kept` | `kept` | `<task>: accepted` |
  | `discarded` | `discarded` | `<task>: branch and worktree removed` |

  `<task>` is the task title the Run carries.
  The `shed` text names memory and quota because a run is also set back for the 5-hour stop of A18 and for a device switched off (A23).
- A24, a pause freezes a worker in place: `RunLine` holds the `Run` and `frozen`, true when the Run's live job has the status `paused`, and a `starting`, `running` or `checking` run that is frozen gets the label `paused` and the summary `<task>: frozen by a pause, continues on resume`.
- A summary is one line: line breaks and control characters in the task title and the concern become spaces, and the line is cut to 160 characters.
- D12, D17: `DispatcherView` has `at_ms`, `mode` (Spec 1's `Mode`), `picked_by` (the Plan's text on where the picks come from), `devices` (id, `enabled: Option<bool>`, the `LineState`, its reason, open slots, `held`, `limits: Vec<Setting>`), `projects` (id, name, device, enabled, `waiting`, `review_cap`), `planned` (device, project, task, provider, account, model, the reason for the pick), `ready_unjudged` (project, task), `workers` (run, device, project, task, state, label, summary, `why`, provider, account, model, start time), `needs_you` (a stable `key`, kind `task`, `account` or `device`, subject, reason), `review` (run, project, task, state, label, summary, branch), `budgets` and `proposals_open`.
  Every struct is `#[serde(default)]`, as `ClusterStatus` is at `crates/idlemax-core/src/protocol.rs:216-218`.
- `dispatcher_view(input: &ViewInput<'_>, now_ms) -> DispatcherView`, where `ViewInput` holds `plan: &Plan`, `runs: &[RunLine]`, `projects: &[ProjectView]`, `devices: &[DeviceView]`, `budgets: &[Budget]`, `days: &[AccountDay]`, `review_cap: u32` (the value of `workers.review_cap`) and `proposals_open: u64`.
- `planned` is every `Pick` of every `DevicePlan` of the Plan, and `devices` takes the line state, reason, open slots and `held` from the same `DevicePlan`.
  A19: `ready_unjudged` is the Plan's list of ready tasks that the orchestrator has not judged, which no slot is filled from; every surface draws it under the label `not judged by the orchestrator`.
- `workers` are the runs in `starting`, `running`, `checking`, `stopped` and `shed`, newest first, and `why` is the Run's `reason` (A5).
- `review` is the runs in `clean`, `needs_you` and `rejected`, newest first; a project's `waiting` counts its runs in `clean` and `needs_you` only, the two states `pace::may_launch` counts against the cap (A24).
- `needs_you` holds, in this order: each `Task` of `Plan.needs_you` (key `task:<project id>:<task id>`, the Task's reason); each account whose `Budget` has the basis `Resting` with `rests_until_ms` later than `day_end_ms`, which is an account out of quota past today (key `account:<name>`, the Budget's `why`); each device whose line state is `Emergency` (key `device:<id>:emergency`, the line's reason); and each device whose `enabled` is `Some(false)` while a project on it is switched on (key `device:<id>:off`, the reason `off, with N projects switched on: idlemax device on <id>`, A23).
  The last kind stays empty until Spec 5 sets `enabled`.
- `budgets` holds per account the `Budget` of today (day start and end, unit, spendable, spent and left percent, why) and up to six earlier account days from `days`, newest first, each with its day start, its `idlemax_pct` as the spent percent, and a spendable percent and why that are `None` while Spec 3's row does not store them (Open Question 3).
  The day is Spec 3's account day, the 24 hours counted back from the weekly reset (A25).
- A23: `devices[].limits` is empty from this plan; Spec 5 fills it with what each other device reports, and the surfaces draw a non-empty list read-only.
  `Setting` (`crates/idlemax-core/src/config/settings.rs:19-26`) gains `Deserialize`, because it now travels to a client.
- `enabled` of a device is `None` until Spec 5 lands, and no surface draws a device toggle for `None`.

**Definition of Done:**

- [x] For each of the ten run states `run_label` and `run_summary` return the text of the table, a frozen `running` run returns `paused`, and a concern with a line break comes back as one line of at most 160 characters.
- [x] `dispatcher_view` lists only `starting`, `running`, `checking`, `stopped` and `shed` runs as workers, newest first, each with its label, its summary and the Run's `reason` as `why`.
- [x] `needs_you` holds exactly the four kinds named above; a hold, a shed, a worker start and an account whose Budget has the basis `Spent` add nothing to it.
- [x] A project with two `clean`, one `needs_you` and one `rejected` run has `waiting` 3.
- [x] An account with a Budget and two `AccountDay` rows shows today and two earlier days, newest first, with the earlier days' spendable percent `None`.
- [x] Verify: `cargo test -p idlemax-core dispatch::label && cargo test -p idlemax-core dispatch::view && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 2: Serve the view and show it in `idlemax status`

**Objective:** Add `GET /v1/dispatcher` on the coordinator and make `idlemax status` print the dispatcher line and the workers with the same labels.
A coordinator without the route leaves `idlemax status` as it is today.

**Files:**

- Create: `crates/idlemax/src/coord/api/dispatcher_view.rs`
- Create: `crates/idlemax/src/cli/out/dispatcher_view.rs`
- Create: `crates/idlemax/tests/fixtures/dispatcher_demo.sql`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/cli/client/dispatch.rs`
- Modify: `crates/idlemax/src/cli/handlers.rs`
- Modify: `crates/idlemax/src/cli/print.rs`
- Modify: `crates/idlemax/src/cli/out/mod.rs`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax/src/coord/api/dispatcher_view.rs` (unit test on an in-memory store), `crates/idlemax/src/cli/print/tests.rs`

**Key Decisions / Notes:**

- D12, D17: the route joins the `v1` router at `crates/idlemax/src/coord/api.rs:81-102`, so it needs the bearer token, and its access is `Read`.
- The handler follows `jobs::status` at `crates/idlemax/src/coord/api/jobs.rs:43-49`: one `app.with_store` call runs Spec 1's `gather_plan`, reads the Runs by state through `coord/store/runs.rs`, the projects with `Store::list_projects`, the devices with `Store::devices` (`crates/idlemax/src/coord/store/devices.rs:152`), the Budgets with Spec 3's `budgets` and the closed account days from `coord/store/budget.rs`, and calls `dispatcher_view`, so one lock gives one consistent picture.
  `frozen` of a `RunLine` comes from the status of the Run's live job in the `jobs` table.
- Hot path: the dashboard polls every 5 s and the TUI every 2 s (`crates/idlemax/assets/dashboard.js:4`, `crates/idlemax/src/tui/feed.rs:20`).
  The handler costs one `gather_plan` call, the same work `GET /v1/plan` does, and it reads at most 200 runs per state group and seven account days per account, never the whole run history.
- `ApiClient::dispatcher()` joins Spec 1's client calls in `crates/idlemax/src/cli/client/dispatch.rs`; `handlers::status` at `crates/idlemax/src/cli/handlers.rs:14-22` asks for the status and the view and treats a 404 as "no dispatcher block", the way `or_empty` does at `crates/idlemax/src/tui/feed.rs:162-169`.
- `print::status` at `crates/idlemax/src/cli/print.rs:77` adds, after the devices table, the line `dispatcher: <mode> | <device>: <line state>: <reason>`, a table `STATE  PROJECT  TASK  MODEL  AGE  SUMMARY`, then `needs you: N` and `waiting for review: N`.
  `--json` prints the `ClusterStatus` fields plus `dispatcher`, and the TOON node lives in the new `cli/out/dispatcher_view.rs`, because `cli/out/views.rs` is already over 400 lines and `cli/out/dispatch.rs` is Spec 1's file for `idlemax plan` and `idlemax project`.
- The fixture `dispatcher_demo.sql` is written against the landed schema of Specs 1 to 3 and of the store today: one device row, two project rows (one on, one off), four runs (`running`, `checking`, `clean`, `needs_you`) with a reason, a branch and a job row each, and the stored state that makes `gather_plan` return one Task with a "needs you" reason.
  Times are written relative to the load, as `strftime('%s','now') * 1000`, and every lease expiry lies a day ahead.
  Task 6 adds the open rule proposal to it.
- `Store` gains `#[cfg(test)] pub fn load_sql(&mut self, sql: &str)` beside `open_in_memory` (`crates/idlemax/src/coord/store.rs:150-153`), and the route test loads the fixture with `include_str!`, so a schema change that breaks the fixture breaks a test.
- Docs: `docs/API.md` gets the route in the coordinator table and a section "The dispatcher view" with the fields and the label table; `README.md` gets the new block under "How much is used right now?" and in the `idlemax status` row of the Controls table.

**Definition of Done:**

- [x] `GET /v1/dispatcher` without a token is 401, and on a store loaded with the fixture it returns two workers with the labels `working` and `checking`, two review rows, one `needs_you` line and two projects.
- [x] `idlemax status` prints the dispatcher line and a workers table whose STATE cells equal `run_label`, and `idlemax status --json` carries the same text in `dispatcher.workers[].label`.
- [x] Against a coordinator without the route, `idlemax status` prints what it prints today and exits 0.
- [x] Verify: `cargo test -p idlemax --bin idlemax coord::api::dispatcher_view && cargo test -p idlemax --bin idlemax cli::print && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 3: Decide who is calling

**Objective:** Add the pure rule that tells an owner session from a process this idlemax launched, and the pure parsers that find the calling process in OS output.
Nothing is enforced yet; Task 4 wires it in.

**Files:**

- Create: `crates/idlemax-core/src/control/caller.rs`
- Modify: `crates/idlemax-core/src/control/mod.rs`
- Modify: `crates/idlemax/src/agent/runner/launch.rs`
- Test: `crates/idlemax-core/src/control/caller.rs` (unit tests at the end of the file)

**Key Decisions / Notes:**

- D12, A12: `LAUNCHED_ENV: &str = "IDLEMAX_JOB_ID"` is the one marker.
  The runner already sets it on every job at `crates/idlemax/src/agent/runner/launch.rs:56`, and Spec 2's `pane_env` passes it into every worker's pane.
  The literal is defined once: `JOB_ID_ENV` at `crates/idlemax/src/agent/runner/launch.rs:17` becomes `pub use idlemax_core::control::caller::LAUNCHED_ENV as JOB_ID_ENV`.
- `Access` is `Read`, `Stop` or `Owner`; `Caller` is `Owner`, `Launched { pid }` or `Unseen`.
- `Mark` is `None`, `ThisHome` or `OtherHome`, and `Link` is `{ pid, mark }`.
  `mark_of(env, homes) -> Mark` reads one process's environment pairs: without `LAUNCHED_ENV` it is `None`; with it and an `IDLEMAX_HOME` value that equals none of `homes` it is `OtherHome`; with it and no `IDLEMAX_HOME`, an empty one or one of `homes` it is `ThisHome`.
  `homes` is this server's state dir as written and as canonical path, so the function does no I/O.
  A marked process without `IDLEMAX_HOME` counts as this home, so the rule fails closed.
- `classify(same_host, chain)`: a caller that is not on this host is `Owner`; a caller on this host whose process was not found (`chain` is `None`) is `Unseen`; a chain that holds a `ThisHome` link, the caller itself included, is `Launched` with that link's pid; anything else is `Owner`.
  An ancestor whose environment cannot be read has the mark `None` and the walk goes on, because macOS hides the environment of its own signed programs (`crates/idlemax/src/agent/runner/os.rs:275-276`) and the agent CLI or the job supervisor above them is readable.
- `OtherHome` exists so that a coordinator in a throwaway home, started by a test that a job or worker of the real idlemax runs, treats its own callers as the owner.
- `permit(caller, access)`: `Read` and `Stop` always pass; `Owner` passes for `Owner` only, so `Unseen` fails closed.
  The refusal is a typed error with two texts: "this call comes from a job or worker that idlemax started (pid N); only the owner can do this, from their own terminal, the dashboard or the TUI" and "idlemax could not find the program that sent this call, so it is not treated as the owner; stop and pause still work".
- `control_access(action)`: `Resume` is `Owner`; `Pause`, `StopAll` and `Cancel` are `Stop`.
- `same_host(peer_ip, local_ips)`: true for a loopback peer or a peer address that is one of this host's interface addresses, so a caller on this host cannot pass as remote by picking another local source address.
- `peer_pid_from_lsof(text, peer)` reads the `-Fpn` output of `lsof`, whose lines start with `p` (a pid), `f` (a descriptor) and `n` (a name), and returns the pid of the `p` block that holds an `n` line starting with the peer's `address:port->`; the other block is the server itself.
- `socket_inode(text, peer, local)` reads `/proc/net/tcp` or `/proc/net/tcp6` text and returns the inode of the row whose local address is the peer and whose remote address is this server; the test rows are copied from a real Linux file when the task is done, not typed by hand.

**Definition of Done:**

- [x] `mark_of` returns `None` without the marker, `ThisHome` with the marker and no `IDLEMAX_HOME`, `ThisHome` with the marker and this home, and `OtherHome` with the marker and another home.
- [x] `classify` returns `Launched` when a `ThisHome` link sits two ancestors up behind an unmarked one, `Owner` for a chain whose only marked links are `OtherHome`, `Owner` for a chain with no marker, `Unseen` for a local caller with no chain, and `Owner` for a caller on another host.
- [x] `permit` passes `Stop` for every caller and refuses `Owner` access for `Launched` and `Unseen`.
- [x] `same_host` is true for a loopback peer and for a peer that equals the second of two local addresses, and false for an address that is not local.
- [x] `peer_pid_from_lsof` picks the client's pid from a two-block sample with `f` lines and returns `None` for empty output.
- [x] `socket_inode` finds the inode in a real IPv4 sample and returns `None` when no row matches.
- [x] Verify: `cargo test -p idlemax-core control::caller && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 4: Refuse owner-only calls from a launched process

**Objective:** Enforce the access table on the coordinator, on the agent local API and in the CLI, so a job or worker that idlemax started cannot start work, resume it, keep its own branch or widen the fence.
The honest list of what this does not stop goes into `README.md`.

**Files:**

- Create: `crates/idlemax/src/coord/api/caller.rs`
- Create: `crates/idlemax/tests/worker_lock.rs`
- Modify: `crates/idlemax/src/coord/server.rs`
- Modify: `crates/idlemax/src/coord/api/http.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/coord/api/jobs.rs`
- Modify: `crates/idlemax/src/coord/api/projects.rs`
- Modify: `crates/idlemax/src/coord/api/plan.rs`
- Modify: `crates/idlemax/src/coord/api/runs.rs`
- Modify: `crates/idlemax/src/agent/api.rs`
- Modify: `crates/idlemax/src/agent/mod.rs`
- Modify: `crates/idlemax/src/agent/runner/launch.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/config_cmd.rs`
- Modify: `crates/idlemax/tests/e2e.rs`
- Modify: `crates/idlemax/tests/config_cmd.rs`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax/tests/worker_lock.rs`, `crates/idlemax/src/coord/api/caller.rs` (unit tests)

**Key Decisions / Notes:**

- D12: `serve_with` drops the peer address today (`crates/idlemax/src/coord/server.rs:75-78`); it now keeps it, and `connection` (`:106-118`) adds an `axum::Extension` that holds the peer address and the local address of the stream to the app clone of that connection, for the coordinator and for the agent local API, which serves through the same function (`crates/idlemax/src/agent/api.rs:115`).
- `coord/api/caller.rs` has `CallerCheck`, built once per server from the server's state dir (as written and canonical) and a lookup function, and carried to handlers as a second `Extension`.
  `CallerCheck::ensure(peer, access)` returns at once for `Read` and `Stop`; for `Owner` it calls the lookup on the blocking pool with a 2 second limit, then `classify` and `permit`.
  The lookup is a plain function value, so the unit tests pass one that returns a fixed chain or nothing.
- The real lookup finds the calling pid on macOS with `/usr/sbin/lsof -nP -a -iTCP@<peer address>:<peer port> -sTCP:ESTABLISHED -Fpn`, and on Linux by reading `/proc/net/tcp`, `/proc/net/tcp6` and the `fd` links under `/proc`.
  It then walks at most 64 parents with `sysinfo`, reading the parent pid and the environment the way `crates/idlemax/src/agent/runner/os.rs:220-235` and `:257-271` do, and turns each environment into a `Link` with `mark_of`.
  For a peer that is not loopback it reads this host's addresses from `sysinfo::Networks::new_with_refreshed_list()` and each interface's `ip_networks()` for `same_host`.
  On an OS with neither lookup the caller counts as `Owner`, marked `// SHORTCUT: no caller lookup on Windows, add one when a Windows device joins`.
- `ApiFailure` at `crates/idlemax/src/coord/api/http.rs:17-40` gains `Forbidden(String)` with status 403.
- Routes of today: `jobs::control` (`crates/idlemax/src/coord/api/jobs.rs:129-142`) calls `ensure` with `control_access(req.action)`; the agent's `control` and `reload` (`crates/idlemax/src/agent/api.rs:123-157`) do the same, with `reload` always `Owner`, and `agent::api::router` takes the state dir so that it can build its `CallerCheck`.
- Routes of Specs 1 and 2, each classed as the access table says: the project handlers in `coord/api/projects.rs` (`on`, the add and `settings` are `Owner`, `off` is `Stop`), `POST /v1/plan/refresh` in `coord/api/plan.rs` (`Owner`), the handler of `POST /v1/dispatcher/mode` that A1 gives Spec 1 (`Stop` when the body names `off`, `Owner` otherwise), and in `coord/api/runs.rs` the start (`Owner`), the stop (`Stop`), keep and discard (`Owner`).
- The runner pins the home: `launch` (`crates/idlemax/src/agent/runner/launch.rs:53-57`) sets `IDLEMAX_HOME` to `paths.root()` after the spec's own `env`, next to the job id, so every plain job names the home that launched it and a job spec cannot name another.
- CLI: `access_of(&Cmd) -> Access` in `crates/idlemax/src/cli/dispatch.rs` is one match with no wildcard arm, so a command added later must be classed.
  `Owner` are `resume`, `local resume`, `config set` and `config unset` (through a new `ConfigArgs::changes_settings`, since `ConfigCmd` is private to `cli/config_cmd.rs`), `mode` toward `propose` or `auto`, `start`, `project on`, `project add`, `project set`, `plan --refresh`, `review keep` and `review discard`.
  `execute` refuses an `Owner` command when its own environment holds `LAUNCHED_ENV`, before the token is loaded and before any call, with a `CliError::runtime` whose help is "ask the owner to run it from their own terminal".
  In a worker's pane every other command ends with the existing "IDLEMAX_TOKEN is set but empty" error (`crates/idlemax/src/state.rs:114-121`), which is what A14 wants.
- The test helpers that start the binary drop the marker, so the suite also passes when a job of idlemax runs it: `spawn` and `run` in `crates/idlemax/tests/e2e.rs` (`:88-104`, `:146-148`) and `idlemax` in `crates/idlemax/tests/config_cmd.rs` (`:18-19`) gain `.env_remove("IDLEMAX_JOB_ID")`.
- The integration test starts a coordinator in a temp home on a free port with the helpers of `crates/idlemax/tests/e2e.rs:75-104`, then runs `idlemax job-wrap --marker <file> -- /usr/bin/env -u IDLEMAX_JOB_ID <idlemax> --coord <url> resume` with `IDLEMAX_JOB_ID=7` on the supervisor, so the calling process is clean and only its parent, the supervisor of `crates/idlemax/src/main.rs:18-27`, carries the marker.
  The other-home case gives the supervisor an `IDLEMAX_HOME` of a second temp dir and the inner command the real one through `env`.
- Docs: `docs/API.md` "Auth and errors" gets the access table, the 403 and the two refusal texts, "Control semantics" says `resume` is owner-only and can now be refused with 403, and the job environment gains `IDLEMAX_HOME`.
  `README.md` "Safety model" gets a part "Who may switch idlemax on" with the rule, the sentence that a worker holds no token, and the list "What the lock does not stop" from the Approach of this plan.

**Definition of Done:**

- [x] With a marked ancestor of this home and a clean calling process, `resume` exits 1 with the refusal text and the coordinator answered 403; the same call with no marked ancestor exits 0.
- [x] With a marked ancestor that names another home, `resume` exits 0, and with a marked ancestor that has no `IDLEMAX_HOME` it is refused.
- [x] With a marked ancestor of this home, `POST /v1/runs/{id}/keep` with `{"merge": true}`, `POST /v1/workers/start`, `POST /v1/dispatcher/mode` toward `auto`, `POST /v1/projects/{id}/on` and `POST /v1/projects/{id}/settings` are 403, and `POST /v1/dispatcher/mode` toward `off` is not refused by the lock.
- [x] With the marker in its own environment, `idlemax resume` and `idlemax config set agent.notify true` are refused before any network call, shown by pointing `--coord` at a closed port and still getting the lock's text; with `IDLEMAX_TOKEN` also set to an empty value the text is still the lock's.
- [x] `POST /v1/local/reload` from a process with a marked ancestor of this home is 403.
- [x] With a lookup that returns nothing for a local caller, `resume` is 403 with the "could not find the program" text and `stop_all` is 200.
- [x] A job the runner starts has `IDLEMAX_HOME` equal to the agent's state dir, also when its spec sets another value.
- [x] Verify: `cargo test -p idlemax --test worker_lock && cargo test -p idlemax --bin idlemax coord::api && cargo test -p idlemax --bin idlemax agent::api && cargo test -p idlemax --bin idlemax agent::runner && cargo test -p idlemax --test e2e && cargo test -p idlemax --test config_cmd && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 5: Change limits through the API

**Objective:** Let the dashboard and the TUI read and change the hard limits through the coordinator, with the same editor that `idlemax config set` uses.
Only keys on an explicit list can be changed this way.

**Files:**

- Create: `crates/idlemax-core/src/config/limits.rs`
- Create: `crates/idlemax/src/coord/api/limits.rs`
- Create: `crates/idlemax/src/files.rs` (the content of `crates/idlemax/src/cli/files.rs`, which is deleted)
- Modify: `crates/idlemax-core/src/config.rs`
- Modify: `crates/idlemax/src/main.rs`
- Modify: `crates/idlemax/src/cli.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/cli/client/dispatch.rs`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax-core/src/config/limits.rs` (unit tests), `crates/idlemax/src/coord/api/limits.rs` (unit test with a temp home), `crates/idlemax/src/files.rs` (the moved tests)

**Key Decisions / Notes:**

- D18.1: `OWNER_LIMITS: &[&str]` holds exact `section.name` keys.
  They are the lines of D14 from Spec 1's `DispatcherConfig` (`dispatcher.hold_mem_used_pct`, `dispatcher.shed_mem_used_pct`, `dispatcher.emergency_mem_used_pct`, `dispatcher.hold_disk_free_mib`, `dispatcher.shed_disk_free_mib`, `dispatcher.emergency_disk_free_mib`, `dispatcher.swap_in_low_pages_per_sec`, `dispatcher.swap_in_high_pages_per_sec`), the owner's quota share from Spec 3 (`quota.owner_reserve_pct`) and the review cap from Spec 2 (`workers.review_cap`).
  The list is explicit because the token alone must never be able to change `agent.command_allowlist` or any `coordinator.*` key from a browser, and the mode is not on it because it has its own route (A1).
- `limit_settings(text, os)` filters `list_settings` (`crates/idlemax-core/src/config/settings.rs:30-65`) to the list; `set_limit(text, key, raw, os)` returns a typed `NotALimit` error for any other key and otherwise calls `set_setting` or `unset_setting` (`crates/idlemax-core/src/config/edit.rs:45-83`), so a value that would stop a start is refused the same way.
- `GET /v1/limits` returns `[Setting]` (`Read`); `POST /v1/limits` takes `{"key": "...", "value": "..."}` with `value: null` for "back to the default" (`Owner`, through `ensure` of Task 4) and returns `{key, before, after, changed, report}`.
- A2: the handler reads `config.toml`, edits it with `set_limit`, and saves it with a backup through `files::save`; it calls no daemon, because each daemon re-reads these keys when the file's mtime changes.
  `report` is the pure `plan_reload(&old, &new)` (`crates/idlemax-core/src/config/reload.rs:50`) computed in the handler, so the reply never depends on a running agent, and a surface shows "applied now" for a key under `applied` and "restart the agent" or "restart the coordinator" for a key under the other two lists.
- The file helpers move one layer in: `crates/idlemax/src/cli/files.rs` becomes `crates/idlemax/src/files.rs`, an adapter the coordinator may use, and `save` and `read_optional` return a typed `FileError` (operation, path, source) in place of `anyhow` (`crates/idlemax/src/cli/files.rs:7`, `:32`, `:41`), because `AGENTS.md` keeps `anyhow` in `main.rs` and the CLI.
  `main.rs` gains `mod files;`, and `cli.rs` replaces `mod files;` with `pub(crate) use crate::files;`, so every caller under `cli/` keeps its path.
- `idlemax config set` already changes any of these keys and needs no change; Task 4 keeps a launched process from using it.
- Docs: `docs/API.md` gets both routes and the reply; `README.md` "Change a setting" says the limits can also be changed in the dashboard and the TUI, and that the limits of another device are changed on that device (A23).

**Definition of Done:**

- [x] No key in `OWNER_LIMITS` starts with `agent.`, `coordinator.` or `advisor.`, and every key is found by `list_settings` for both OS values.
- [x] `set_limit` on `agent.command_allowlist` and on `dispatcher.mode` returns `NotALimit`, and on a listed key with a bad value returns the editor's `Invalid` error with its reason.
- [x] `POST /v1/limits` with a valid value rewrites `config.toml` in the temp home, leaves a backup, and returns `changed: true` with the key under `report.applied`; with `value: null` the key leaves the file.
- [x] `POST /v1/limits` for a key off the list is 400 and the file is unchanged.
- [x] The tests that were in `cli/files.rs` pass in `files.rs`, and `idlemax config set` still writes its backup.
- [x] Verify: `cargo test -p idlemax-core config::limits && cargo test -p idlemax --bin idlemax coord::api::limits && cargo test -p idlemax --bin idlemax files:: && cargo test -p idlemax --test config_cmd && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 6: Store, list and approve rule proposals

**Objective:** Keep the lines the orchestrator proposes for `rules.md` after a rejected or discarded task, let the owner list, approve or reject them, and append an approved line to the file.
Nothing reaches `rules.md` without an approval.

**Files:**

- Create: `crates/idlemax-core/src/dispatch/proposal.rs`
- Create: `crates/idlemax/src/coord/store/proposals.rs`
- Create: `crates/idlemax/src/coord/api/proposals.rs`
- Modify: `crates/idlemax-core/src/dispatch/mod.rs`
- Modify: `crates/idlemax-core/src/dispatch/orchestrator.rs`
- Modify: `crates/idlemax-core/src/dispatch/prompt.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `crates/idlemax/src/coord/api/plan.rs`
- Modify: `crates/idlemax/src/coord/store/schema.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/cli/client/dispatch.rs`
- Modify: `crates/idlemax/tests/fixtures/dispatcher_demo.sql`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax-core/src/dispatch/proposal.rs`, `crates/idlemax-core/src/dispatch/orchestrator/tests.rs`, `crates/idlemax/src/coord/store/proposals.rs`, `crates/idlemax/src/coord/api/proposals.rs`

**Key Decisions / Notes:**

- D18.4: `Proposal` has `id`, `at_ms`, `run`, `line`, `reason`, `status` (`open`, `approved`, `rejected`) and `decided_ms`.
- `admit(run, line, reason, run_state, rules_text, earlier)` accepts a proposal only for a run in `rejected` or `discarded`, only one per run, and only a line that is 1 to 200 characters, one line, without control characters, not already a line of `rules.md` and not equal to any earlier proposal whatever its status; at most 20 may be open, and a refused proposal is dropped with one `Info` event.
- `append_rule(rules_text, line)` returns the text with the line added as the last line, prefixed with `- ` when the last non-empty line of the file starts with `- `, and returns the text unchanged when the line is already there, so approving twice appends once.
- Migration: the next `Vn` in `crates/idlemax/src/coord/store/schema.rs`, which ends at V7 today (`:4`).
  Specs 1, 2 and 3 each add one before this plan, so V11 is the number to expect; check `MIGRATIONS` on `main` first and take the next free number.
  One table: `rule_proposals (id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, run INTEGER NOT NULL UNIQUE, line TEXT NOT NULL UNIQUE, reason TEXT NOT NULL, status TEXT NOT NULL CHECK (status IN ('open','approved','rejected')), decided_ms INTEGER) STRICT`.
- Bound: `Store::prune` (`crates/idlemax/src/coord/store.rs:208`) drops `approved` rows older than 30 days, whose line `rules.md` now holds, and keeps at most 500 `rejected` rows, the newest.
- Routes: `GET /v1/proposals` (`Read`, open ones first), `POST /v1/proposals/{id}/approve` and `POST /v1/proposals/{id}/reject` (`Owner`); a finished proposal is 409 and an unknown id 404.
- Approve reads the file `Paths::rules_file()` names, writes the new text with `files::save`, then marks the row; if the row update fails the next approve finds the line present and only marks the row.
  It answers 409 with "rules.md is at its 16 KiB limit: shorten it, then approve" when the new text would pass the size `rules_file::read` clips at, because a line beyond the clip would never reach a pass.
- Intake, in Spec 1's pass.
  `summary` in `crates/idlemax-core/src/dispatch/prompt.rs` gains `finished`: the five newest runs in `rejected` or `discarded` that have no proposal yet, each with its run id, project, task title, state and concern, which `step` in `crates/idlemax/src/dispatch/mod.rs` reads through `coord/store/runs.rs`.
  `prompt` gains one sentence: after a rejected or discarded task in `finished` the pass may propose one line for the rules, with the run id and one sentence of reason.
  `schema` and `validate` in `crates/idlemax-core/src/dispatch/orchestrator.rs` accept one optional field `rule_proposal: {"run", "line", "reason"}`, `PassResult` carries it, and a proposal whose run is not in `finished` is dropped and named in `dropped`.
  `ingest_pass` in `crates/idlemax/src/coord/api/plan.rs` hands it to `admit` and stores the row.
- No pass is started for a proposal: the fingerprint and `pass_due` of Spec 1 are not changed, so the proposal rides on the next pass that A20 allows.
- The fixture of Task 2 gains one `rejected` run and one open proposal for it.
- Docs: `docs/API.md` gets the three routes and the new field of the pass output; `README.md` gets a short part on proposals next to where Spec 1 describes `rules.md`.

**Definition of Done:**

- [x] `admit` refuses a proposal for a `clean` run, a second proposal for the same run, a two-line text, a line already in `rules.md` and a line that was rejected before.
- [x] `validate` accepts an output with `rule_proposal` for a run in `finished`, drops one for an unknown run and names it in `dropped`, and still rejects the whole output for an unknown field.
- [x] A coordinator test ingests a fixture envelope that carries a proposal and then finds one open proposal for that run in `GET /v1/proposals`.
- [x] Approving an open proposal adds exactly one line to `rules.md` in the temp home, and a second approve of the same id is 409 with the file unchanged.
- [x] Rejecting leaves `rules.md` byte for byte as it was.
- [x] A store opened at the previous schema version migrates and keeps its rows.
- [x] Verify: `cargo test -p idlemax-core dispatch::proposal && cargo test -p idlemax-core dispatch::orchestrator && cargo test -p idlemax --bin idlemax coord::store::proposals && cargo test -p idlemax --bin idlemax coord::api::proposals && cargo test -p idlemax --bin idlemax plan && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 7: Decide and store the three kinds of notice

**Objective:** Add the pure decisions for what interrupts the owner (a finished task, something that needs the owner, one daily summary) and the coordinator sweep step that turns them into stored notices.
Everything else produces no notice.

**Files:**

- Create: `crates/idlemax-core/src/notify/notice.rs`
- Create: `crates/idlemax/src/coord/store/notices.rs`
- Create: `crates/idlemax/src/coord/notices.rs`
- Modify: `crates/idlemax-core/src/notify.rs`
- Modify: `crates/idlemax/src/coord/store/schema.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/coord/mod.rs`
- Modify: `crates/idlemax/src/coord/sweep.rs`
- Test: `crates/idlemax-core/src/notify/notice.rs`, `crates/idlemax/src/coord/notices.rs` (unit tests in each file)

**Key Decisions / Notes:**

- D17, the three notification kinds (a shared name this spec provides): `NoticeKind` is `task_finished`, `needs_you` or `daily_summary`, and `Notice` has `id`, `at_ms`, `kind`, `title`, `body`.
- What is worth a notice.
  A run that finished with the verdict `clean` or `needs_you` is a `task_finished` item with the key `run:<id>`.
  A22: a plain job that ended `succeeded`, `failed`, `killed` or `parked` is a `task_finished` item with the key `job:<id>`.
  `is_plain_job(spec)` is true when the spec has no `worker` section, is not the advisor's pass (`advice::ai::is_ai_pass`, `crates/idlemax-core/src/advice/ai.rs:29`) and its name does not start with `dispatch-`, the prefix of every job Spec 1 queues (`dispatch-setup:`, `dispatch-check:`, `PASS_JOB_NAME`).
  Each line of `DispatcherView.needs_you` is a `needs_you` item under its own key.
  A worker start, a move to another provider, a hold, a shed, a pick, a stopped worker job and a `rejected` run are nothing, the last because D8 says a rejected task is never reported as done.
- `finished_batch(waiting, now_ms)` with `FINISH_BATCH_MS = 600_000`: it returns nothing until the oldest waiting item finished 10 minutes ago, then one draft for every waiting item that finished within those 10 minutes, so each message covers tasks that finished within 10 minutes of each other and no task waits longer than 10 minutes.
- Text of a `task_finished` notice: one run gives the title `idlemax: <project>` and the body `<what was done> - <label>`, where "what was done" is the Run's one-line summary when it has one and `run_summary` otherwise; one plain job gives `idlemax: <job name>` and the body of `notify::job_result_label` (`crates/idlemax-core/src/notify.rs:13`); several items give `idlemax: N tasks finished` and one body line per item, `<project>: <what was done> - <label>` or `<job name>: <result>`.
  Labels come from `run_label`.
- `needs_batch(new_lines)` puts every "needs you" line that has no covered key into one draft per sweep: one line gives `idlemax needs you: <subject>` and the reason; several give `idlemax needs you: N things` and one body line per line, `<subject>: <reason>`, at most ten, then `and N more: idlemax status`.
- `daily_summary(facts, now_ms, utc_offset_secs)` with `SUMMARY_HOUR = 9` (A22: 09:00 in the coordinator's local time): once the local time passes 09:00 it returns one draft with the key `daily:<local date>` for the 24 hours that ended at 09:00.
  The body is `Tasks done: N` (runs that reached `clean` or `needs_you` in those hours), one line per account from its last closed account day, `<account>: spent X%` with ` of Y% budget` added when that day's spendable percent is stored (Open Question 3), and `Waiting for review: N`.
  It returns nothing when no task was done, nothing was spent and nothing waits.
- A key is covered once, and a "needs you" key is released when its line has been out of the view for `NEEDS_REARM_MS = 3_600_000`, so an account that runs out again next week and an emergency stop an hour after the last one notify again, while a condition that flickers does not.
- Migration: the next `Vn` after Task 6's, V12 if nothing else landed; check `main` first.
  Two tables: `notices (id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('task_finished','needs_you','daily_summary')), title TEXT NOT NULL, body TEXT NOT NULL) STRICT` with an index on `at_ms`, and `notice_items (key TEXT PRIMARY KEY, notice INTEGER NOT NULL, at_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL) STRICT, WITHOUT ROWID` with an index on `at_ms`.
- `coord::notices::sweep` runs inside `sweep_once` (`crates/idlemax/src/coord/sweep.rs:72-85`) every 15 s; a failure is logged and does not stop the lease sweep, like the schedule step at `:76-78`.
  It reads the runs and the plain jobs that finished in the last 24 hours without a covered key (`finished_jobs_since` in `coord/store/notices.rs` selects them from `jobs` by `finished_ms`), the "needs you" lines of `dispatcher_view`, and the day facts with the offset from `chrono::Local`, as `crates/idlemax/src/agent/runner/finish.rs:53` reads it.
  It stamps `last_seen_ms` on every "needs you" key still in the view, deletes those gone for an hour, and writes each notice with its keys in one transaction.
- Bounds: `Store::prune` drops notices and items older than 30 days and keeps the newest 1000 notices.

**Definition of Done:**

- [x] A worker start, a provider move, a hold, a shed, a pick, a stopped worker job and a rejected run give no item, and `is_plain_job` is false for a spec with a `worker` section, for the advisor's pass and for a job named `dispatch-check:app`.
- [x] Three runs that finished at minute 0, 4 and 9 give no draft at minute 9 and one draft with three lines at minute 10; a run at minute 12 is not in it and gets its own draft at minute 22.
- [x] A run and a plain job that finished two minutes apart give one draft titled `idlemax: 2 tasks finished` with one line each.
- [x] Thirty new "needs you" lines in one sweep give one draft with ten lines and `and 20 more: idlemax status`.
- [x] `daily_summary` returns nothing at 08:59 local, the summary at 09:00, and nothing for an empty day.
- [x] Two sweeps over the same finished run store one notice; an emergency that clears and returns after 30 minutes stores one `needs_you` notice, and one that returns after 61 minutes stores a second.
- [x] Verify: `cargo test -p idlemax-core notify::notice && cargo test -p idlemax --bin idlemax coord::notices && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 8: Deliver a notice to the desktop and the inbox

**Objective:** Carry stored notices to every device, where each becomes one inbox entry, and one desktop notification where `agent.notify` is true (A22).
The per-job desktop notification goes away, so only the three kinds interrupt.

**Files:**

- Create: `crates/idlemax/src/agent/notices.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Modify: `crates/idlemax-core/src/job_run/inbox.rs`
- Modify: `crates/idlemax-core/src/notify.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax/src/agent/notify.rs`
- Modify: `crates/idlemax/src/agent/mod.rs`
- Modify: `crates/idlemax/src/agent/event_loop/coord.rs`
- Modify: `crates/idlemax/src/agent/event_loop/jobs.rs`
- Modify: `crates/idlemax/src/agent/runner/finish.rs`
- Modify: `crates/idlemax/src/cli/inbox.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/out/lists.rs`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax-core/src/job_run/inbox.rs` (unit test), `crates/idlemax/src/agent/notices.rs` (unit test with a temp home), `crates/idlemax/tests/logs_inbox.rs`, `crates/idlemax/src/coord/api/tests.rs`

**Key Decisions / Notes:**

- D17: `Heartbeat` gains `notices_after: Option<i64>`, written like `keep_awake` at `crates/idlemax-core/src/protocol.rs:33-35`, and `HeartbeatReply` gains `notices: Vec<Notice>`; an old agent sends nothing and an old coordinator returns an empty list.
- A22: the agent always sends `notices_after` (`heartbeat()` at `crates/idlemax/src/agent/event_loop/coord.rs:76-87`), with the id from its cursor file or 0 when it has none, whatever `agent.notify` says.
- The coordinator's `heartbeat` handler (`crates/idlemax/src/coord/api/agents.rs:26-39`) returns at most 20 notices with a larger id, oldest first, and never one older than 24 hours, so a new device gets no old backlog.
- `apply_reply` (`crates/idlemax/src/agent/event_loop/coord.rs:39-49`) hands the notices to `agent::notices::deliver`.
  It writes one inbox note per notice with a temp file and a rename, as `write_note` does at `crates/idlemax/src/agent/runner/finish.rs:117-137`, then shows one desktop notification for the whole reply when `agent.notify` is true (the notice's own title and body for one notice, `idlemax: N notices` and `see idlemax inbox` for several), and then writes the highest id to `agent_state_dir()/notices.cursor`.
- `crates/idlemax/src/agent/notify.rs` keeps its `osascript` call in one function `show(&Notification)`; other systems get the inbox note only, as today (`:14-16`).
- The per-job notification is removed: `reap_with` (`crates/idlemax/src/agent/event_loop/jobs.rs:141-149`) no longer calls `notify_job_finish`, because a finished plain job is now a line of the `task_finished` batch (Task 7).
  `notify_job_finish` and `notify::job_finish_notification` (`crates/idlemax-core/src/notify.rs:26-37`) lose their only caller and are deleted, and their three tests assert `job_result_label` instead.
- A worker job never writes a job note, because the note of a finished task is the notice: `write_note` is skipped for a spec with a `worker` section wherever Spec 2's worker manager (`crates/idlemax/src/agent/worker/mod.rs`) ends a job through `crates/idlemax/src/agent/runner/finish.rs`, and a test with Spec 2's `FakeHost` pins that the inbox stays empty.
  A plain job keeps its own note with the output tail, which the notice does not carry.
- Inbox: the note is `<local date>-n<id>-<kind>.md` with `# <title>`, `- Notice: <id> (<kind>)`, `- At: <stamp>` and the body.
  `parse_note` (`crates/idlemax-core/src/job_run/inbox.rs:155-236`) reads the `- Notice:` and `- At:` lines, and `InboxEntry` (`:143-152`) gets `job_id: Option<JobId>` and `notice: Option<i64>`, each left out of the JSON when empty, so the JSON of a job note does not change.
- `idlemax inbox` shows `n<id>` in the JOB column for a notice, and `idlemax inbox show n<id>` prints its note; `InboxCmd::Show` (`crates/idlemax/src/cli/commands.rs:110-113`) takes the id as text, and a bad id is a usage error that names both forms.
  `lists::inbox` (`crates/idlemax/src/cli/out/lists.rs:326-336`) keeps its four columns, and the `job` cell holds the job id for a job note and the text `n<id>` for a notice; the file is at 402 lines, so the change adds no line to it.
- Docs: `docs/API.md` "Heartbeat and leases" gets both fields; `README.md` gets the three kinds, the inbox form and the new meaning of `agent.notify` in "Following a job" and in the settings table (`README.md:444`, `:1000`).

**Definition of Done:**

- [x] A heartbeat with `notices_after: 0` gets the stored notice once; the next heartbeat with its id gets none; a heartbeat without the field gets none.
- [x] After `deliver` with `agent.notify` false, the temp home's inbox holds one note for the notice, the cursor file holds its id, no notification was shown, and delivering the same notice again leaves one note.
- [x] With `agent.notify` true, a reply with one notice asks `show` once with the notice's title, and a reply with three asks it once with `idlemax: 3 notices`.
- [x] A finished plain job and a stopped worker job both leave `show` uncalled in `reap_with`, and the worker job leaves no job note.
- [x] `idlemax inbox` lists a job note and a notice note together, newest first, and `idlemax inbox show n1` prints the notice note.
- [x] `idlemax inbox --json` for a job note has the same keys as before this task.
- [x] Verify: `cargo test -p idlemax-core job_run::inbox && cargo test -p idlemax-core notify && cargo test -p idlemax --bin idlemax agent::notices && cargo test -p idlemax --test logs_inbox && cargo test -p idlemax --bin idlemax coord::api && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 9: Add the dispatcher controls and views to the dashboard

**Objective:** Give the browser dashboard the main switch, the project toggles, the live worker list, the "needs you" and review lists, the budget, the limits and the rule proposals.
It is verified by TS-001 to TS-006.

**Files:**

- Modify: `crates/idlemax/assets/dashboard.html`
- Modify: `crates/idlemax/assets/dashboard.js`
- Modify: `README.md`
- Modify: `docs/assets/dashboard-light.png`
- Modify: `docs/assets/dashboard-dark.png`
- Test: `crates/idlemax/src/coord/api/tests.rs` (the existing dashboard test)

**Key Decisions / Notes:**

- D12, D17: a Dispatcher card goes first in the left column of the grid at `crates/idlemax/assets/dashboard.html:399-425`, with a three-button mode switch (`Off`, `Propose`, `Auto`, a `radiogroup`), the line state and reason of each device, a `Stop workers` button, a `Start` button while any worker is `stopped by you`, the Workers table (label, project, task, model, age, summary, and the Run's reason behind a `details` element), and "Would launch next" from `planned`.
  Under "Would launch next" the card draws `ready_unjudged` with the label `not judged by the orchestrator` and no control (A19), and the `picked_by` line.
- The right column at `:426-442` gets the cards Needs you, Waiting for review, Budget, Projects, Limits and Rule proposals.
  Waiting for review has no action button and one line that says keep and discard are `idlemax review keep <run>` and `idlemax review discard <run>`.
  A device toggle is drawn only when the view has more than one device and `enabled` is set.
- `refresh()` (`crates/idlemax/assets/dashboard.js:131-164`) adds `GET /v1/dispatcher`, `GET /v1/limits` and `GET /v1/proposals`; a 404 hides the dispatcher cards, so the page still works against an older coordinator.
- Every card is drawn through `section()` (`:240-252`) and the `h()` helper, never through an HTML sink, which the test at `crates/idlemax/src/coord/api/tests.rs:1061-1071` checks; the page keeps one script (`tests.rs:1037-1041`) and the policy of `crates/idlemax/src/coord/web.rs:19-21` is unchanged.
- Calls, each through `act()` (`:186-200`): the mode switch posts to `POST /v1/dispatcher/mode`, `Start` to `POST /v1/workers/start`, `Stop workers` to `POST /v1/workers/stop` with the global scope of `scopes.global()` (`:118-123`), a project toggle to `POST /v1/projects/{id}/on` or `/off`, Save and "default" to `POST /v1/limits`, Approve and Reject to the proposal routes.
  `Auto`, `Start`, a project going on and Approve ask first through `confirmBox()` (`:203-212`); `Stop workers` asks once; off, a project going off and Reject do not ask.
  A 403 from the lock shows as the red toast `act()` already makes from the reply's `error`, because `api()` treats only 401 as a login failure (`:113`).
- Limits: one row per limit of `GET /v1/limits` with the value in a labelled input, the default beside it, Save and "default" buttons, and the reply's "applied now", "restart the agent" or "restart the coordinator" under the row.
  A device whose `limits` list in the view is not empty gets its rows as text with no input and the line "change these on that device with idlemax config set" (A23).
- Typing survives a poll: `section()` restores focus for `button, select, summary` today (`:245`), and that list gains `input`; the Limits card keeps each row's unsaved text in a map by key and puts it back after a rebuild.
- Budget: per account, a bar for today with the text `spent X% of Y%` and its why as the title, and one small bar per earlier day with `spent X%`, and ` of Y%` when the day's spendable percent is set, built on the `.meter` element of `quotaWindow()` (`:728-747`); with no account the card says "no AI account is configured".
- The label text is always shown beside its colour; every control is a `button` or a labelled `input`; the new cards follow the page's existing phone rules.
- Docs: `README.md` "Web dashboard" lists the new cards and what can be switched from them; the two screenshots are taken again from the throwaway home after the fixture of Task 2 is loaded.

**Definition of Done:**

- [x] TS-001 to TS-006 pass in a browser against the throwaway home, and the page has no console error.
- [ ] The cards read correctly at a phone width and in the light and the dark theme.
- [x] Text typed into a limit input is still there, with the focus, after the next poll.
- [x] Against a coordinator that answers 404 to `GET /v1/dispatcher`, the page shows today's cards and no empty dispatcher card.
- [x] The dashboard test still finds one script tag and no HTML sink, and the script parses.
- [x] Verify: `cargo test -p idlemax --bin idlemax the_dashboard_runs_one_same_origin_script && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 10: Add the Dispatch tab to the TUI

**Objective:** Give the TUI the same views and controls in one new tab, and a stop key that works on every tab.
Every screen still fits 80x24.

**Files:**

- Create: `crates/idlemax/src/tui/dispatcher.rs`
- Create: `crates/idlemax/src/tui/dispatcher_view.rs`
- Create: `crates/idlemax/src/tui/snapshots/dispatch-80x24.txt`
- Modify: `crates/idlemax/src/tui.rs`
- Modify: `crates/idlemax/src/tui/app.rs`
- Modify: `crates/idlemax/src/tui/app/keys.rs`
- Modify: `crates/idlemax/src/tui/feed.rs`
- Modify: `crates/idlemax/src/tui/view.rs`
- Modify: `crates/idlemax/src/tui/table.rs`
- Modify: `crates/idlemax/src/tui/snapshots.rs`
- Modify: `crates/idlemax/src/tui/snapshots/tabs-80x24.txt`, `empty-80x24.txt`, `states-80x24.txt`, `why-80x24.txt`, `report-80x24.txt`, `wide-characters-80x24.txt`, `why-120x40.txt`, `wide-120x40.txt`
- Modify: `README.md`
- Test: `crates/idlemax/src/tui/snapshots.rs`, `crates/idlemax/src/tui/app/tests.rs`

**Key Decisions / Notes:**

- D12: `Tab::Dispatch` with the title `Dispatch` becomes the seventh entry of `Tab::ALL` (`crates/idlemax/src/tui/app.rs:19-36`), `tables` grows to seven (`:79`), `rows` and `target` get its arm (`:134-143`, `:152`), and the digit keys run from 1 to 7 (`crates/idlemax/src/tui/app/keys.rs:30`); the seven titles without counts take 79 of 80 cells in `tab_titles` (`crates/idlemax/src/tui/layout.rs:123-138`).
- The tab shows one pane at a time, switched with `v`: workers, would launch, needs you, review, budget, projects, limits, rules.
  Each pane is a `table::Spec` (`crates/idlemax/src/tui/table.rs:12-22`) with its flex and drop columns, under a block whose title carries the mode and the line state, and the selected row's summary and reason fill a details box below it, the way `render_advice` splits its area at `crates/idlemax/src/tui/view.rs:256-279`.
  The "would launch" pane lists `planned`, then `ready_unjudged` with the label `not judged by the orchestrator` (A19), and the limits pane lists a device's read-only `limits` after the editable ones, marked `read-only` (A23).
- `Snapshot` (`crates/idlemax/src/tui/feed.rs:27-37`) gains the view, the limits and the proposals, fetched with the 404 rule of `or_empty` (`:162-169`); `Request` (`:41-52`) gains stop, start, set mode, project switch, set limit, approve rule and reject rule, each one client call in `perform` (`:171-188`).
  The calls are the client methods of Specs 1 and 2 behind `idlemax stop`, `idlemax start`, `idlemax mode` and `idlemax project on|off`, and the methods Tasks 5 and 6 add.
- Keys: `x` on every tab asks "Stop every worker now? They continue only after start." and sends stop; on the Dispatch tab `m` moves to the next mode, `g` sends start, `t` switches the selected project, `e` edits the selected limit, and `a` and `d` approve and reject the selected proposal.
  `m` toward `propose` or `auto`, `g`, `t` toward on and `a` ask first through the existing confirm box (`crates/idlemax/src/tui/view.rs:362-381`); off, stop after its one question, and reject do not.
  On this tab `a` and `d` are matched before the advice arms at `crates/idlemax/src/tui/app/keys.rs:54-55`, and `p` and `r` act on everything, as they do on the Advice and Events tabs.
- `e` opens a one-line input in the footer: typed characters extend the value, Backspace removes one, Enter sends it, Esc closes it, and the input is handled before every other key, as the confirm box is at `crates/idlemax/src/tui/app/keys.rs:14-22`, so `q` and Esc do not quit while it is open.
- `key_hints` (`crates/idlemax/src/tui/view.rs:330-360`) gains `x stop workers` at priority 1 on every tab, lists the Dispatch tab with Advice and Events at `:333`, and reads `tab/1-7 switch`.
- A refusal from the lock arrives as the red flash the TUI already shows for a failed request.
- `crates/idlemax/src/tui/view.rs` is at 381 lines, so all drawing of the new tab lives in `dispatcher_view.rs`; the two files are named after `DispatcherView`, apart from `crates/idlemax/src/cli/dispatch.rs`, which routes commands.
- Snapshots are accepted with `UPDATE_SNAPSHOTS=1 cargo test -p idlemax --bin idlemax tui::` (`crates/idlemax/src/tui/snapshots.rs:3`) and read before the commit.
- Docs: `README.md` "Terminal dashboard" gets the tab, the panes and the new keys in the key table.

**Definition of Done:**

- [x] `dispatch-80x24.txt` holds every pane, the stop question, the limit input, a read-only limit row and a lock refusal, each within 80 columns and 24 rows.
- [x] `tab_titles` for the seven titles fits 80 cells, and the six regenerated 80x24 snapshots and the two 120x40 ones were reviewed.
- [x] `x` then `y` on the Jobs tab yields the stop request, and `x` then any other key yields none.
- [x] On the limits pane, `e`, typed text that holds a `q` and Enter yield one set limit request with that text, and Esc yields none and does not quit.
- [x] On the rules pane `a` then `y` yields the approve request and no advice request.
- [x] Without a dispatcher view from the coordinator, the tab says so in one line and no key on it sends a request.
- [x] Verify: `cargo test -p idlemax --bin idlemax tui:: && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 11: Add `idlemax mcp`

**Objective:** Add an MCP server over stdio that is a thin client of the coordinator API, so an agent the owner instructs can read status, start, stop, switch a project and list finished work.
A process that idlemax launched gets no on-switch from it.

**Files:**

- Create: `crates/idlemax-core/src/mcp.rs`
- Create: `crates/idlemax/src/cli/mcp.rs`
- Create: `crates/idlemax/tests/mcp.rs`
- Create: `scripts/test-mcp-clients.sh`
- Modify: `crates/idlemax-core/src/lib.rs`
- Modify: `crates/idlemax/src/cli.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/home/render.rs`
- Modify: `skills/idlemax/SKILL.md`
- Modify: `README.md`
- Test: `crates/idlemax-core/src/mcp.rs` (unit tests), `crates/idlemax/tests/mcp.rs`

**Key Decisions / Notes:**

- D12, `idlemax mcp` (a shared name this spec provides): the JSON-RPC layer is written by hand and no dependency is added.
  The server needs five methods (`initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`) and `serde_json` is already in both crates, while the SDK crate would add a service framework and schema macros for six tools, against the rule in `AGENTS.md` on abstractions.
- Wire: one JSON object per line on stdin and on stdout, and nothing else on stdout; logs go to stderr.
  `initialize` answers `{"protocolVersion": ..., "capabilities": {"tools": {}}, "serverInfo": {"name": "idlemax", "version": VERSION}}` with the client's version when it is `2025-06-18` or `2024-11-05`, and `2025-06-18` otherwise.
  The revision `2025-03-26` is left out, because it has a server accept a batch, an array on one line, which this parser does not read; a newer revision joins the list only after its tools and stdio parts are read on modelcontextprotocol.io and found unchanged.
- `idlemax_core::mcp` is pure: `parse(line)` returns the message kind, and builders return the reply text; an unparsable line is error `-32700` with a null id, an unknown method `-32601`, an unknown tool or bad arguments `-32602`.
- Tools, each with an object `inputSchema` and no extra properties: `status`, `start`, `stop`, `project_on` (`name`), `project_off` (`name`), `finished_work`.
  `status` makes the `status()` and `dispatcher()` calls and returns the text `idlemax status` prints into a pipe; `start`, `stop`, `project_on` and `project_off` call the client methods behind `idlemax start` and `idlemax stop` (Spec 2) and `idlemax project on|off` (Spec 1, `crates/idlemax/src/cli/client/dispatch.rs`), which find a project's id by its name; `finished_work` returns the `review` list of the view.
- A tool that fails returns `isError: true` with the message and the next step the CLI would print; the handshake and `tools/list` need no token and no coordinator, so an agent CLI's health check passes on a device that is not set up, and the first tool call says `idlemax setup`.
- The lock: `start` and `project_on` check `LAUNCHED_ENV` in the server's own environment first and return the refusal text of Task 3 with no network call, and the coordinator checks again through Task 4, because the server's ancestors include the process that started it.
  A worker's agent CLI that starts the server reaches no route in any case, because its pane holds an empty `IDLEMAX_TOKEN` (A14).
- `Cmd::Mcp` joins `crates/idlemax/src/cli/commands.rs:13-95`, gets its arm in `access_of` (`Read`, because the tools carry their own check) and is routed in `execute` before the state dir is read; started with a terminal on stdin it prints what it is for and the four add lines to stderr and exits 2.
- How the owner adds it, checked against each CLI's own `--help` (Claude Code 2.1.322, codex-cli 0.166.0, opencode 2.0.30, agy 1.5.4): `claude mcp add --scope user idlemax -- idlemax mcp`, `codex mcp add idlemax -- idlemax mcp`, `opencode mcp add --global idlemax -- idlemax mcp`, `agy mcp add idlemax idlemax mcp`.
- `scripts/test-mcp-clients.sh` proves the four lines with the real CLI binaries and sends no prompt, so it spends no quota and starts no session.
  It makes a temp dir, sets `HOME` and `IDLEMAX_HOME` inside it with the assert of `AGENTS.md`, unsets `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `OPENCODE_CONFIG_DIR` and `XDG_CONFIG_HOME`, runs each add line and that CLI's `mcp list`, checks that `idlemax` is listed, and skips a CLI that is not installed.
  After each add it fails when no file under the temp dir changed, which means the CLI wrote its settings somewhere else, and it removes the temp dir at the end.
- Docs: `COMMANDS` at `crates/idlemax/src/cli/home/render.rs:18-57` gains `idlemax mcp`, `skills/idlemax/SKILL.md` is regenerated with `idlemax skill > skills/idlemax/SKILL.md`, and `README.md` "Use it from an AI agent" gets the server, its six tools and the four add lines, plus a row in the Controls table.

**Definition of Done:**

- [x] Over a pipe, `initialize`, `notifications/initialized`, `tools/list` and `tools/call status` get correct replies, `tools/list` names exactly the six tools, and every stdout line parses as JSON.
- [x] `initialize` with the version `2025-03-26` is answered with `2025-06-18`.
- [x] The `status` reply text holds the label of a seeded worker.
- [x] With `IDLEMAX_JOB_ID` in the server's environment, `start` and `project_on` return `isError: true` with the lock's text, shown with `--coord` at a closed port.
- [x] In a home with no token, the handshake succeeds and `tools/call status` returns `isError: true` naming `idlemax setup`.
- [ ] `sh scripts/test-mcp-clients.sh` passes for each agent CLI installed on the machine, and `idlemax skill --check` passes.
- [x] Verify: `cargo test -p idlemax-core mcp && cargo test -p idlemax --test mcp && cargo test -p idlemax --test skill && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 12: Needs the owner's OK: switch control and visibility on for real

**Objective:** Do the steps that change the owner's real machine and spend real quota, each only after the owner says yes to that step, and record what was proven.
No code changes here.

**Files:**

- Modify: `docs/ROADMAP.md`

**Key Decisions / Notes:**

- D12, D17: every step below touches the real home, a real setting or real quota, so none of it runs in a code task and none of it runs without the owner's yes for that step.
- Step 1: with the build that holds Tasks 1 to 11 installed the way roadmap item 31 describes, run `idlemax config set agent.notify true` on the Mac.
- Step 2: run the add line of Task 11 in each of the owner's agent CLIs that the owner names, and check each with that CLI's `mcp list`.
- Step 3: with one project switched on and the mode `auto`, let one small task on the standard tier run to its verdict; this is the only quota this task may spend.
  Watch the worker in the dashboard and in the TUI, see one desktop notification and one inbox entry for the finished task, see none for the worker start, and read the Budget card and pane against `idlemax quota`.
- Step 4, the lock from a real launched process: the owner adds `env` and the installed `idlemax` binary to `agent.command_allowlist` for this step and removes both after it.
  Queue three plain jobs and read each log: `idlemax start` and `idlemax mode auto` end with the lock's text from the CLI, and `env -u IDLEMAX_JOB_ID idlemax start` ends with the coordinator's 403 text that names a job or worker.
  Nothing started and the mode did not change.
- Step 5: from one of the owner's own agent sessions, call the MCP tools `status`, `stop` and `start`.
- Step 6: open the dashboard in Safari, in Firefox and on a phone and check the new cards, which is the open part of roadmap item 19 for this spec.
- The result goes under item 36 of `docs/ROADMAP.md` as a dated "Done" note with what was seen in each step.

**Definition of Done:**

- [ ] The owner said yes to each step before it ran, and a step the owner declined is recorded as not done.
- [ ] One finished task produced exactly one desktop notification and one inbox entry on the Mac.
- [ ] The three jobs of step 4 were refused, two by the CLI and one by the coordinator, and `agent.command_allowlist` is back to what it was.
- [ ] An owner session read status and switched the dispatcher off and on through `idlemax mcp`.
- [ ] Item 36 of `docs/ROADMAP.md` records the date and what each step showed.
- [ ] Verify: `idlemax doctor && idlemax status && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

## Open Questions

1. **The lock is a second layer, not a fence (A13, pending).**
   A job or worker runs as the owner's OS user: it inherits `HOME` and `IDLEMAX_HOME` (`crates/idlemax-core/src/job_run/env.rs:9-27`), so it can read the token file and write `config.toml`, whose dispatcher keys apply at once (A2), and it can detach from its ancestors in one line.
   D12 says a worker "can never" start idlemax or raise a limit, and no task of any spec can make that true without a boundary the OS enforces at launch.
   Recommended: the owner's answer to A13 gives workers a sandbox profile or an OS user that cannot read or write the state dir, built by Spec 2; until then the README of Task 4 calls the lock a guard against accidents and lists what it does not stop.
2. **A worker's pane names no home.**
   The lock counts a marked process for this home unless its environment names another one, and Spec 2's `pane_env` passes `IDLEMAX_JOB_ID` and `IDLEMAX_JOB_TAG` but not `IDLEMAX_HOME` (`docs/plans/2026-11-11-dispatcher-2-workers.md:318`).
   So a worker that runs the tests of the idlemax repository itself gets a 403 from the test's own coordinator for `resume` (`crates/idlemax/tests/e2e.rs:254`), while a plain job does not, because Task 4 pins the variable in the runner.
   Recommended: Spec 2 adds `IDLEMAX_HOME` with the agent's state dir to `pane_env`; it is an existing variable, so A12's "no second marker" holds, and until then the rule fails closed.
3. **The budget of earlier days is not stored.**
   D17 asks for the quota spent against the budget for today and for previous days, and Spec 3's `quota_days` row keeps `total_pct`, `idlemax_pct`, `owner_pct` and `runs` with no spendable percent and no why (`docs/plans/2026-11-11-dispatcher-3-quota.md:311-319`).
   Recommended: Spec 3 writes `spendable_pct` and `why` into the row when `close_quota_days` closes a day; until then an earlier day and the daily summary show `spent X%` without the budget, as Tasks 1, 7 and 9 build.

## Review Disposition

1. fixed differently: A1 gives the route `POST /v1/dispatcher/mode` and the command `idlemax mode` to Spec 1, so this plan calls them and classes the route in Task 4 instead of building it.
2. fixed: the Approach states what a file write and a detach reach, A2 is named as the reason, and D12's gap is Open Question 1 under A13; A2 settled how a limit reaches a running daemon.
3. fixed: the access table classes every write route of Specs 1 and 2, the CLI check is an exhaustive match, and Task 4 tests keep with merge.
4. fixed: per A14 no task claims that a worker can stop idlemax; Truth 3, Task 11 and Task 12 prove the refusals only, and Task 4 pins that the lock's text comes before the token error.
5. fixed: per A22 the per-job notification is removed, a finished plain job joins the batch, and a worker job writes no job note (Tasks 7 and 8).
6. fixed: per A12 `IDLEMAX_JOB_ID` is the only marker, defined once in the core.
7. fixed differently: A12 allows no second marker, so the lock is scoped by the existing `IDLEMAX_HOME` of the marked process, the runner pins it, and the test helpers drop the marker; what is left for pane workers is Open Question 2.
8. fixed differently: the status block gets its own new file `cli/out/dispatcher_view.rs`, so Spec 1's `cli/out/dispatch.rs` is not touched.
9. fixed differently: a "needs you" key is released after its line has been out of the view for an hour, which needs no episode id that the view does not have.
10. fixed differently: all new "needs you" lines of one sweep share one notice; no question is left, because D7 puts an unready task on the "needs you" list and D17 item 2 notifies for it.
11. fixed: the day is Spec 3's account day (A25), earlier days carry an optional budget, and the missing column is Open Question 3.
12. fixed: the proposal names its run, the pass input gains the finished runs, Spec 1's files are in the list, and approve uses `Paths::rules_file()`.
13. fixed: `files.rs` moves to the crate level with a typed error, and the limits handler calls no agent.
14. fixed: the handler computes `plan_reload` itself.
15. fixed: `cli/out/lists.rs` is in Task 8 with the content of the `job` cell.
16. fixed: per A22 the inbox entry is always written and only the desktop notification follows `agent.notify`.
17. fixed: `same_host` takes this host's interface addresses.
18. fixed: the fixture `dispatcher_demo.sql` is created in Task 2, checked by a test, and named in every scenario; scenarios that needed a live worker or an AI account now expect what a throwaway home can show.
19. fixed: per A5 `why` is the Run's `reason` and the fallback is gone.
20. fixed: the two 120x40 snapshots and the accept command are named.
21. fixed: V11 and V12 are the expected numbers.
22. fixed differently: the revision `2025-03-26` is not offered, so no batch has to be read.
23. fixed: the rule now forbids a prompt or a session, and the script fails when an add changed no file under the temp dir.
24. fixed: `input` joins the focus list and the Limits card keeps unsaved text.
25. fixed: `Store::prune` bounds `rule_proposals`.
26. fixed: the core files live under Spec 1's `dispatch` module, and the TUI and CLI files are named after `DispatcherView`.
27. fixed: Task 2 states the cost of the handler and its read bounds.
28. implementation (Task 1): `ViewInput` has no `devices` field; the devices come from the Plan's `DevicePlan`s, `DeviceLine.enabled` stays `None` and `limits` empty, and Spec 5 adds the field when it sets them. The `device:<id>:off` kind is built and tested through `device_needs` and fires once `enabled` is set.
29. implementation (Task 6): `admit` takes the pass's `RuleProposal` (run, line, reason) as one value, `admit(&proposal, run_state, rules_text, earlier)`, rather than the three fields apart. It also refuses a line that starts with `- ` or `#`, which would change the shape of the owner's file, and a reason with a control character; `check_lengths` bounds the proposal's reason like every other reason.
30. implementation (Task 7): a day is checked once, at the first sweep after 09:00; an empty day covers its `daily:<date>` key with no notice, so a task that finishes later that day sends no "summary" of the 24 hours before 09:00. Notice items are pruned by `last_seen_ms`, so a condition that lasts a month stays covered.
31. implementation (Task 9): "Stop workers" falls back to `stop_all` on `POST /v1/control` while `POST /v1/workers/stop` answers 404 (Spec 2 Task 10), and says so; "Start" explains its 404. A refused limit keeps the typed text and the row shows the stored value as `now <value>`. A test checks every path the script calls against the router, with the worker and device routes named as pending. The two screenshots are not retaken: the fixture home has one offline device and no account, so it would show less than the current pictures; they are retaken with the browser run of TS-001 to TS-006, which also proves the phone, theme and focus items. That run could not happen on the build host (the headless browser lacks system libraries), so those three boxes stay open.
