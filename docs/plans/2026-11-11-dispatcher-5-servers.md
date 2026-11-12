# Dispatcher 5: Servers Implementation Plan

Created: 2026-11-11
Agent: Claude Code
Status: IN_PROGRESS
Approved: Yes
Iterations: 1
Worktree: No
Type: Feature

## Summary

**Goal:** The owner's Linux servers run workers around the clock under one coordinator that lives on an always-on server, every device is switched on by the owner and held to the lines of its profile, two devices on the same login spend one Budget, and each server's agent holds a credential that opens nothing but the agent routes.

This plan builds roadmap item 37, the decisions D19 and D20, and the parts the Answers A11, A15, A22 and A23 give to Spec 5.

## Out of Scope

- The macOS readings, the room rule itself, Projects with their `--device` flag, Tasks, the Plan, the orchestrator pass and the check job on the base branch belong to Spec 1 (`idlemax_core::dispatch`, `crates/idlemax/src/dispatch/`); this plan feeds the room rule on Linux, gates the dispatcher per device and decides when a base check runs again.
- The Herdr host, the Run, `idlemax start` and `idlemax stop` belong to Spec 2; this plan adds the second host behind its seam and the device scope of its stop operation.
- The Budget arithmetic belongs to Spec 3; this plan passes it one input and never computes a Budget.
- The dashboard, the TUI, `idlemax mcp` and the caller check belong to Spec 4; this plan serves `DeviceView.enabled`, the two device routes, the call behind the device toggle and the read-only limits of other devices (A23).
- The device switch does not hold plain command jobs the owner queued: builds, tests and backups keep `pause`, `resume` and the guard, while every job the dispatcher created stops with the device (A11).
- Changing another device's limits from the coordinator: a device's limits change on that device with `idlemax config`, and the dashboard shows them read-only (A23).
- A sandbox or a separate OS user for workers: the owner's answer is pending (A13, Open Question 1), and Spec 2 builds it.
- Account identity for old AI jobs (a `cost = ai` job with no `worker` section): they keep their rules unchanged (A8).
- Headless Codex, OpenCode and agy workers: only Claude has a proven way to name and continue a print-mode session (Open Question 4).
- A quota reader for print-mode runs (Open Question 3).
- The Linux idle-sleep inhibitor of roadmap item 29: a server has no suspend timer to hold off, and a Linux laptop is not part of D19.

## Approach

**Chosen:** Extend what exists instead of adding a server mode: `DeviceProfile` grows to the three Profiles with the two old names kept as input aliases, Spec 1's `WorkerRoom::evaluate` gains a Linux branch fed by one pure sizing step in `dispatch/linux.rs`, the device switch is one column on `devices` and one pure gate that the Plan, the feed reply, the pass and every lease ask, the account identity is one field of `DeviceInfo` checked by pure rules on the coordinator, the headless runner is a second `WorkerHost` that writes to the job log, and a device credential is a second kind of bearer token that the coordinator maps to one device.
**Why:** Every new rule is a pure function over facts the heartbeat already carries, so the coordinator needs one migration and the agents need no new protocol; the cost is that the coordinator must be upgraded before any agent, because an older coordinator cannot read the new profile names, and that every server needs one credential step at setup.

## Context for Implementer

Specs 1 to 4 land before this one (D21), and this plan uses their code by these names; read the merged code for the exact signature before writing a line that uses one.

| What this plan uses | Where it lives |
|---|---|
| `Mode`, the route `POST /v1/dispatcher/mode`, `idlemax mode <off|propose|auto>` | `crates/idlemax-core/src/dispatch/mod.rs`, `crates/idlemax/src/coord/api/mode.rs` (Spec 1, `auto` by Spec 2, A1) |
| `LineState`, `RoomDecision`, `RoomPolicy`, `WorkerRoom::evaluate`, `lines_verdict` | `crates/idlemax-core/src/dispatch/room.rs` (Spec 1) |
| `DispatcherConfig` and the `[dispatcher]` keys | `crates/idlemax-core/src/config/dispatcher.rs` (Spec 1) |
| `Agent::on_sample`, `Agent.room`, the field `worker_room` | `crates/idlemax/src/agent/event_loop/sample.rs` (Spec 1) |
| `LiveConfig::current()`, `AppState::live()` (A2) | `crates/idlemax/src/live_config.rs`, `crates/idlemax/src/coord/api.rs` (Spec 1) |
| `Project`, `ProjectView`, `idlemax project list|on|off|add|set` with `--device`, `GET /v1/projects?device=<id>` | `crates/idlemax-core/src/dispatch/project.rs`, `crates/idlemax/src/cli/project.rs`, `crates/idlemax/src/coord/api/projects.rs` (Spec 1) |
| `DispatchReply`, the route `POST /v1/agent/dispatch-inputs` | `crates/idlemax-core/src/dispatch/inputs.rs`, `crates/idlemax/src/coord/api/plan.rs` (Spec 1) |
| `build`, `DevicePlan.held`, `Plan.notes` | `crates/idlemax-core/src/dispatch/plan.rs` (Spec 1) |
| `pass_due` | `crates/idlemax-core/src/dispatch/orchestrator.rs` (Spec 1) |
| `BaseCheck`, `next_step`, `check_job`, `discover` | `crates/idlemax-core/src/dispatch/check.rs`, `crates/idlemax-core/src/dispatch/task.rs` (Spec 1) |
| `gather_plan`, `step` | `crates/idlemax/src/dispatch/mod.rs` (Spec 1) |
| `is_dispatcher_job`, `lease_lines` | `crates/idlemax-core/src/types.rs`, `crates/idlemax/src/coord/api/agents.rs` (Spec 1) |
| `WorkerHost` (`start`, `state`, `send`, `stop`, `end_text`), `HostState`, `FakeHost` | `crates/idlemax/src/agent/worker/host.rs` (Spec 2) |
| `Workers`, `Workers::recover`, the saved worker record, the launch phases, the `Commands` seam | `crates/idlemax/src/agent/worker/mod.rs`, `record.rs`, `steps.rs`, `cmd.rs` (Spec 2) |
| `agent_args`, `pane_env`, `env_problems` | `crates/idlemax-core/src/worker/launch.rs` (Spec 2) |
| `WorkersConfig` and the `[workers]` keys | `crates/idlemax-core/src/config/workers.rs` (Spec 2) |
| `pace::may_launch`, `control::plan_stop`, `RunState::may_become` | `crates/idlemax-core/src/worker/pace.rs`, `control.rs`, `run.rs` (Spec 2) |
| `dispatch::launch_due`, `dispatch::stop_workers`, `POST /v1/workers/stop` | `crates/idlemax/src/coord/dispatch.rs`, `crates/idlemax/src/coord/api/runs.rs` (Spec 2) |
| `Budget`, `BudgetBasis`, `BudgetInput.owner_reserve_override_pct`, `QuotaConfig` | `crates/idlemax-core/src/quota/budget.rs`, `crates/idlemax-core/src/quota/config.rs` (Spec 3) |
| `budgets(store, cfg, views, now)` and its tests | `crates/idlemax/src/coord/api/budget.rs`, `crates/idlemax/src/coord/api/budget/tests.rs` (Spec 3) |
| `Access`, `LAUNCHED_ENV`, `same_host` | `crates/idlemax-core/src/control/caller.rs` (Spec 4) |
| `CallerCheck::ensure(peer, access)`, `ApiFailure::Forbidden`, the peer address extension | `crates/idlemax/src/coord/api/caller.rs`, `crates/idlemax/src/coord/api/http.rs`, `crates/idlemax/src/coord/server.rs` (Spec 4) |
| `access_of(&Cmd)` | `crates/idlemax/src/cli/dispatch.rs` (Spec 4) |
| `DispatcherView`, `dispatcher_view`, `ViewInput`, the "needs you" key `device:<id>:off` | `crates/idlemax-core/src/dispatch/view.rs` (Spec 4) |
| `OWNER_LIMITS`, `limit_settings`, `POST /v1/limits` | `crates/idlemax-core/src/config/limits.rs`, `crates/idlemax/src/coord/api/limits.rs` (Spec 4) |
| The Dispatch tab, `Request`, `perform` | `crates/idlemax/src/tui/dispatcher.rs`, `crates/idlemax/src/tui/dispatcher_view.rs`, `crates/idlemax/src/tui/feed.rs` (Spec 4) |

A worker starts only when every fence allows it: no `$IDLEMAX_HOME/PAUSE` file, no pause, mode `auto`, no owner stop, the device on, the project on, the line state `run`, the account usable on that device, and the Budget usable.
This plan adds three fences: the device switch, the account identity and the server's own credential.
It also weakens one, and says so: once Task 11 moves the coordinator off the Mac, Spec 4's caller check no longer sees the process behind a call from the Mac, so a Mac worker that reads the owner token file reaches every owner route with one request.
The CLI and `idlemax mcp` still refuse an owner action under the marker `IDLEMAX_JOB_ID` on every host, because that check is local; what is lost is the second layer against a hand-made HTTP call.
That gap closes only with the OS boundary of A13, so the dispatcher mode on the moved coordinator stays `off` or `propose` until the owner has answered it (A15, Open Question 1).
Every new rule fails closed: a reading that is missing, a login that cannot be read, a device the coordinator does not know and a server without its own credential all mean "start nothing", with a reason that names the next step.

Every test and every manual check runs in a throwaway home with the assert of `AGENTS.md`, on spare ports, with stub agent CLIs first on `PATH`.
No dev run configures a real AI account, starts a real `claude`, `codex`, `agy`, `opencode` or `herdr`, or runs `launchctl` or `systemctl` against a real session.
The repository is public: no address, host name or device id of the owner's machines goes into code, tests, examples or docs.

## File Structure

- `crates/idlemax-core/src/types/device.rs` (modify) - the three Profiles and `DeviceInfo.claude_login`.
- `crates/idlemax-core/src/config/guard_policy.rs` (modify) - the `laptop`, `workbox` and `production` presets and the workbox sizing.
- `crates/idlemax-core/src/dispatch/linux.rs` (create, pure) - the Linux numbers per profile, and the percent and the budget the memory line reads.
- `crates/idlemax-core/src/control/device.rs` (create, pure) - what a device switch changes, `off_reason`, `device_gate`.
- `crates/idlemax-core/src/control/credential.rs` (create, pure) - `Identity`, `AgentAuth`, which routes a device credential opens, which device it may speak for.
- `crates/idlemax-core/src/quota/login.rs` (create, pure) - `Login`, the readers of the Claude and Codex status text, `keep_last_good`.
- `crates/idlemax-core/src/quota/identity.rs` (create, pure) - one login per account name, the same-reading warning, the server-only rule.
- `crates/idlemax-core/src/worker/free_checks.rs` (create, pure) - when a device with a spent budget runs its projects' base checks again.
- `crates/idlemax-core/src/host.rs` (modify) - `listen_scope`: loopback, private or public.
- `crates/idlemax/src/agent/cgroup.rs` (modify) - the memory the agent's cgroup uses.
- `crates/idlemax/src/agent/metrics/login.rs` (create, adapter) - asks `claude auth status --json` for the login.
- `crates/idlemax/src/agent/worker/headless.rs` (create, adapter) - the headless `WorkerHost`.
- `crates/idlemax/src/coord/store/schema.rs`, `store/devices.rs` (modify) - the one migration of this plan and the device columns.
- `crates/idlemax/src/coord/store/credentials.rs` (create, adapter) - the `device_credentials` rows.
- `crates/idlemax/src/coord/api/devices.rs` (create, adapter) - `POST /v1/devices/{id}/on`, `/off` and the credential routes.
- `crates/idlemax/src/cli/device.rs` (create, delivery) - `idlemax device on|off|credential`.
- `crates/idlemax/src/doctor/logins.rs` (create, adapter) - the login checks.
- `crates/idlemax/tests/devices.rs` (create) - the device commands against a real coordinator in a temp home.
- `examples/config.workbox.toml` (create) - a sample config for a work server.
- `docs/ROLLOUT.md`, `docs/API.md`, `README.md`, `docs/WORKER-PROOF.md`, `docs/ROADMAP.md`, `skills/idlemax/SKILL.md` (modify) - docs.

## Assumptions

- Specs 1 to 4 have landed with the names in the table above, with `Admission::Lines` for the dispatcher's own jobs (A16) and with `[dispatcher]`, `[workers]` and the new `[quota]` keys read through `LiveConfig` (A2) - every code task depends on this.
- `orgId` in the reply of `claude auth status --json` names one login - Tasks 5 and 6 depend on this (Open Question 2).
- A print-mode Claude session that was killed in the middle of a turn can be continued with `--resume` - Task 8 depends on this, and Task 12 proves it.
- A Claude session in a Herdr pane on a server refreshes the statusline capture as it does on the Mac, so a server's Claude account keeps a trusted reading - Truth 1 depends on this, and Task 12 proves it (Open Question 3).
- `chrono::Local`, which Spec 4's daily summary reads, follows the `TZ` variable of the coordinator's process - Task 11 depends on this and checks it on the first summary.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Every bearer that is not the owner token costs one store lookup of its digest, so a client on the private network can force one query per request | Low | Low | The coordinator binds only a private address (Task 10) and the lookup is one indexed equality; cache misses for a short time if the logs ever show it |
| A new agent reports `workbox` to a coordinator that predates this plan and every heartbeat is refused | Medium | Medium | The old names stay valid on input (Task 1 test), Task 11 upgrades the coordinator first, and `docs/ROLLOUT.md` states the order |
| The Linux numbers are wrong for a real server and a workbox thrashes or idles | Medium | High | A missing reading only holds, the numbers are settings, and Task 12 records pressure and available memory under load before any server stays in `auto` |
| A worker on a server or on the Mac reads a token file and calls an owner route on the remote coordinator | Low | High | A server holds only its device credential, which opens the agent routes alone, and a server that still sends the owner token from another host is held (Task 9); the Mac keeps the owner token, so the mode on the moved coordinator stays `off` or `propose` until the owner has answered A13 (Task 11, Open Question 1) |
| A print-mode worker never refreshes the quota reading, so its account goes stale | High | Medium | The Budget closes an account with no trusted reading (Spec 3), Task 12 installs the statusline capture on servers that use panes, and it records what a server pane refreshes |
| The unit's task limit of 256 (`packaging/linux/idlemax-agent.user.service:29`) is too low for several headless workers | Medium | Medium | The unit renderer writes `TasksMax=4096` for a `workbox` (Task 1 test), and Task 12 records the unit's `pids.peak` with three workers and changes the constant when the peak is above half of it |
| This plan and another claim the same migration number | High | Low | Specs 1 to 4 take `V8` to `V12`, so Task 3 expects `V13`, reads `MIGRATIONS` on `main` first, takes the next free number and uses exactly one number for this whole plan |
| An upgrade leaves the Mac off and the owner does not notice | Medium | Low | Spec 4's "needs you" entry for an off device with switched-on projects appears once `enabled` is served (Task 3 test), the dashboard draws the device toggle for a lone device too (Task 4), and `README.md` names `idlemax device on <id>` in the upgrade notes |

## Goal Verification

### Truths

1. With the laptop's agent stopped, a `workbox` device that is on still gets Plan picks from an orchestrator pass that ran on a server, and in `auto` it launches workers and reaches verdicts, because the coordinator and the pass no longer need the laptop.
2. Two devices that report the same Claude login under one account name show one Budget, and a Run on either lowers the same `left_pct`; a device that reports another login under that name gets no worker on that account.
3. On a device that is off, no worker, no orchestrator pass and no setup or check job starts, whatever the mode, the Budget, the profile or the host, while a plain job the owner queued for it still runs under the guard.

## Progress Tracking

- [x] Task 1: Add the laptop, workbox and production profiles
- [x] Task 2: Feed the Linux readings into the worker room
- [ ] Task 3: Switch devices on and off on the coordinator
- [ ] Task 4: Add idlemax device on and off, the device toggle and the read-only limits
- [x] Task 5: Read each device's Claude login and check the logins in doctor
- [ ] Task 6: Keep one Budget per login and pace a server-only account
- [ ] Task 7: Run the base checks again when the day's budget is spent
- [ ] Task 8: Add the headless runner
- [x] Task 9: Give each device its own credential
- [x] Task 10: Refuse a public coordinator address and write the server rollout
- [ ] Task 11: Needs the owner's OK: move the coordinator to the always-on server
- [ ] Task 12: Needs the owner's OK: join both servers as workbox and prove the dispatcher there

> Source of truth for completion: `spec-implement` toggles `[ ]` to `[x]` here.

## Implementation Tasks

### Task 1: Add the laptop, workbox and production profiles

**Objective:** Replace the two guard preset names with the three Profiles and say how each maps onto the presets that exist.
A device that names no profile is `laptop` on macOS and `production` everywhere else, and `workbox` exists only where the owner sets it.

**Files:**

- Modify: `crates/idlemax-core/src/types/device.rs`
- Modify: `crates/idlemax-core/src/config/guard_policy.rs`
- Modify: `crates/idlemax-core/src/config.rs`
- Modify: `crates/idlemax-core/src/scheduler/blockers.rs`
- Modify: `crates/idlemax-core/src/explain/reasons.rs`
- Modify: `crates/idlemax-core/src/install/setup.rs`
- Modify: `crates/idlemax-core/src/install/service.rs`
- Modify: `crates/idlemax-core/src/install/service/render.rs`
- Modify: `crates/idlemax/src/agent/event_loop.rs`
- Modify: `crates/idlemax/src/cli/init_cmd.rs`
- Modify: `crates/idlemax/src/cli/setup.rs`
- Modify: `crates/idlemax/src/cli/service.rs`
- Modify: `crates/idlemax/src/cli/service/target.rs`
- Modify: `crates/idlemax/assets/dashboard.js`
- Modify: `examples/config.toml`, `examples/config.server.toml`, `examples/jobs/repo-health-project-a.toml`
- Create: `examples/config.workbox.toml`
- Modify: `crates/idlemax/tests/examples.rs`
- Modify: `README.md`, `docs/API.md`, `docs/ROLLOUT.md`, `docs/ROADMAP.md`
- Test: `crates/idlemax-core/src/config.rs` (inline tests), `crates/idlemax-core/src/scheduler/tests.rs`, `crates/idlemax-core/src/install/setup/tests.rs`, `crates/idlemax-core/src/install/service/tests.rs`, `crates/idlemax/tests/setup.rs`, `crates/idlemax/tests/service_render.rs`

**Key Decisions / Notes:**

- `DeviceProfile` (`types/device.rs:28-35`) becomes `Laptop`, `Workbox` and `Production`, written as `laptop`, `workbox` and `production`, the shared name Profile (D19).
  `Laptop` takes the serde alias `macos_laptop` and `Production` the alias `linux_server`, so every existing `config.toml` and every heartbeat of an older agent still reads.
  An older coordinator cannot read the new names, so the coordinator is upgraded before any agent; `docs/API.md` and `docs/ROLLOUT.md` say so.
- `default_for` (`types/device.rs:39-44`) returns `Laptop` on macOS and `Production` on every other system, which is the rule "a server idlemax knows nothing about starts as `production`" (D19).
  No function returns `Workbox` unless the config or a flag names it, and no host name is ever compared in code (D20).
- The mapping onto the presets: `GuardPolicy::macos_laptop()` is renamed `laptop()` and `linux_server()` is renamed `production()`, with every number unchanged (`config/guard_policy.rs:86-162`).
  `production` keeps `server_sized_for` (`guard_policy.rs:184-205`), the fallback for an unknown cgroup limit (`guard_policy.rs:215-224`) and only `cpu_light` and `io` plain jobs (`scheduler/blockers.rs:89-95`): the services come first and idlemax gets a quarter of the memory at most (D19).
- `GuardPolicy::workbox()` is `production()` with three changes for the plain jobs the owner queues there.
  The two `load_per_cpu` checks are off, because a machine that workers fill as designed carries their load all day.
  `psi_mem_some_pause` is 10.0 and `psi_mem_full_kill` is 5.0, the values at which Task 2 holds and sheds workers on a `workbox`, in place of 5.0 and 1.0 (`guard_policy.rs:151-152`); with the `production` values a plain job would be paused and killed long before a worker is held.
  Swap-in and available memory keep the `production` values, because both sit beyond the worker lines of Task 2.
  `workbox_sized_for` gives plain jobs all the memory but a reserve of 1024 MiB and the agent's own 128 MiB (`AGENT_OVERHEAD_MIB`, `guard_policy.rs:67`), never under 512 MiB, and all CPUs but one, bounded by the memory slots as `server_sized_for` does; a key set in `[guard]` wins (D19: "almost all of its memory and CPU, keeping a small reserve").
  `cost_allowed(Workbox, _)` is true for every cost class, so Spec 1's check job (`cost = cpu_heavy`) and its orchestrator pass (`cost = ai`) run on a workbox while the laptop sleeps.
- `Config::guard_policy_for` (`config.rs:175-188`) gets the third arm, and `Agent::new` (`agent/event_loop.rs:113-124`) treats every profile but `Laptop` on Linux as a server for the unknown-cgroup fallback.
- `idlemax init --profile` (`cli/init_cmd.rs:26-39`) takes the three names and keeps today's two spellings as hidden aliases.
  `idlemax setup` gains `--profile` (`cli/setup.rs:31-56`), carried in `Request` and used by `config_for` (`install/setup.rs:224-235`) in place of the OS default; this is how the owner sets `workbox` at setup (D20).
- The offline unit render (`cli/service/target.rs:30-32`) accepts a target config whose profile is `production` or `workbox`, refuses `laptop` with `target config must use production or workbox`, and writes the real profile into the header line (`target.rs:61`).
- The task limit follows the profile: `ServiceEnv` (`install/service.rs:80-93`) gains `agent_tasks_max: u32`, set next to `agent_ram_mib` (`cli/service.rs:282`, `cli/service/target.rs:46-47`) from the new pure `tasks_max(profile)`, which is the constant `WORKBOX_TASKS_MAX` (4096) for `workbox` and 256 for the other two.
  `sized_unit` (`install/service/render.rs:48-85`) rewrites the `TasksMax=` line with it, as it does for `MemoryHigh`, `MemoryMax` and `CPUQuota`; 256 is too low for several headless workers with their tools, and Task 12 records the real peak (A25).
- `osAndProfile` in `crates/idlemax/assets/dashboard.js:384-389` hid the OS when the profile name began with it; no new name does, so it always shows the OS and then the profile.
- The rename reaches every file that names a preset: the production code above, and the tests in `guard/tests.rs`, `explain/tests.rs`, `report/tests.rs`, `capacity/tests.rs`, `agent/tests.rs`, `agent/event_loop/refill_tests.rs`, `agent/event_loop/reload_tests.rs`, `coord/api/tests.rs`, `tui/snapshots.rs`, `tests/service_render.rs` and `tests/config_cmd.rs`.
  Tests that pin an old name as output are updated (`config/edit.rs:268-269`, `config/settings.rs:138`), and inputs that use an old name stay as they are to prove the alias (`doctor.rs:370`, `scripts/verify-linux.sh:1532`).
- `examples/config.workbox.toml` shows a work server's config with `profile = "workbox"`, a placeholder coordinator name and no real address, and `tests/examples.rs` parses it like the other examples.
- Docs in this task: the profile sentences and the guard table of the README (`README.md:236`, `:683-684`, `:695`, `:996`) with a `workbox` column, the setup sample (`README.md:194`), the profile values in `docs/API.md`, the comments in the example configs and in `examples/jobs/repo-health-project-a.toml:13`, `docs/ROLLOUT.md:178` and the Windows note at `docs/ROADMAP.md:275`.

**Definition of Done:**

- [x] `Config::parse` reads `profile = "linux_server"` as `Production` and `profile = "macos_laptop"` as `Laptop`, and `to_toml_string` writes `production` and `laptop`.
- [x] `DeviceProfile::default_for(Os::Linux)` is `Production` and `default_for(Os::Macos)` is `Laptop`.
- [x] On 8 CPUs and 16 GiB, `guard_policy_for` gives 7 jobs and 15232 MiB for `workbox` and 7 jobs and 4096 MiB for `production`; on 2 CPUs and 2 GiB `workbox` gives 1 job and 896 MiB.
- [x] `GuardPolicy::workbox()` has `psi_mem_some_pause` 10.0, `psi_mem_full_kill` 5.0 and no `load_per_cpu` check, and `production()` equals what `linux_server()` returned before.
- [x] `cost_allowed(Workbox, Ai)` is true and `cost_allowed(Production, CpuHeavy)` is false.
- [x] A rendered agent unit holds `TasksMax=4096` for a `workbox` target and `TasksMax=256` for a `production` one, and a `laptop` target config is refused with a message that names `production` and `workbox`.
- [x] `idlemax setup --role agent --profile workbox --dry-run` in a temp home prints `agent.profile workbox`, and without `--profile` on Linux the planned config holds `production`.
- [x] `examples/config.workbox.toml` parses.
- [x] Verify: `cargo test -p idlemax-core config && cargo test -p idlemax-core scheduler && cargo test -p idlemax-core guard && cargo test -p idlemax-core explain && cargo test -p idlemax-core report && cargo test -p idlemax-core capacity && cargo test -p idlemax-core install && cargo test -p idlemax --bin idlemax agent:: && cargo test -p idlemax --bin idlemax tui::snapshots && cargo test -p idlemax --bin idlemax coord::api::tests && cargo test -p idlemax --test setup --test examples --test service_render --test config_cmd && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 2: Feed the Linux readings into the worker room

**Objective:** Give Spec 1's `WorkerRoom::evaluate` its inputs on Linux: memory pressure stall information, available memory and the cgroup limit, with numbers per profile.
The same three lines then hold on every system, and a reading that cannot be taken only holds new workers.

**Files:**

- Create: `crates/idlemax-core/src/dispatch/linux.rs`
- Modify: `crates/idlemax-core/src/dispatch/mod.rs`
- Modify: `crates/idlemax-core/src/dispatch/room.rs`
- Modify: `crates/idlemax-core/src/dispatch/room/tests.rs`
- Modify: `crates/idlemax-core/src/config/dispatcher.rs`
- Modify: `crates/idlemax/src/agent/cgroup.rs`
- Modify: `crates/idlemax/src/agent/event_loop/sample.rs`
- Modify: `docs/API.md`, `README.md`
- Test: `crates/idlemax-core/src/dispatch/linux.rs` (inline tests), `crates/idlemax-core/src/dispatch/room/tests.rs`, `crates/idlemax/src/agent/cgroup.rs` (inline tests)

**Key Decisions / Notes:**

- The sampler already reads memory pressure, the swap-in rate, available memory and free disk on Linux (`types/device.rs:57-83`, `agent/metrics/linux.rs:171-191`), and `MetricsSnapshot` already carries `psi_mem_some_avg10` and `psi_mem_full_avg10` (`types/device.rs:78-79`), so the one new reading is what the agent's cgroup uses.
  `cgroup.rs` gains `memory_used`, which reads `memory.current` minus `inactive_file` of `memory.stat` (cgroup v2) or `memory.usage_in_bytes` minus `total_inactive_file` (v1), in the directory whose limit `hierarchy_limit` found to be the tightest (`cgroup.rs:47-91`); page cache is taken out because the kernel gives it back before it kills.
- `dispatch/linux.rs` is pure and turns the facts into the one figure the memory line reads, the percent of what idlemax may fill that is in use, and the size of that budget (D19, D14):

  ```rust
  pub struct LinuxNumbers {
      pub reserve_mib: u64, pub share_mib: Option<u64>,
      pub psi_some_hold_pct: f64, pub psi_full_shed_pct: f64,
  }
  pub struct LinuxFacts {
      pub mem_total_bytes: u64, pub mem_available_bytes: u64,
      pub cgroup_limit_bytes: Option<u64>, // set only when new workers would run inside the agent's cgroup
      pub cgroup_used_bytes: Option<u64>,  // what that cgroup uses, when it has a limit
      pub outside_used_bytes: u64,         // tracked jobs and workers that run outside that cgroup
      pub tracked: u32,                    // dispatcher jobs and workers this agent tracks
  }
  pub struct LinuxRoom { pub used_pct: f64, pub budget_mib: u64 }
  pub fn linux_numbers(profile: DeviceProfile, guard: &GuardPolicy, cfg: &DispatcherConfig) -> LinuxNumbers;
  pub fn linux_room(facts: &LinuxFacts, numbers: &LinuxNumbers) -> Result<LinuxRoom, String>;
  ```

  The share is the smaller of `share_mib` and the cgroup limit, when either is set.
  idlemax's own use is `cgroup_used_bytes` plus `outside_used_bytes`.
  The budget is the total minus the reserve, capped by the share; the room is the available memory minus the reserve, capped by the share minus idlemax's own use, never below zero; `used_pct` is 100 times one minus room over budget.
  It is an `Err` with the missing reading named when the total is zero, the budget is zero, or a cgroup limit is set while the cgroup's use is unknown.
- What the agent passes (`Agent::on_sample`, `agent/event_loop/sample.rs:21-47`): the total and the available memory of the sample, and the rest from what it tracks.
  When the agent's cgroup has a limit (`MemoryCap::Limited`, read at start), `cgroup_used_bytes` is `memory_used` and `outside_used_bytes` is the `rss_bytes` of the tracked jobs and workers that run outside that cgroup: plain jobs under `Wrapper::SystemdScope` (`job_run/wrap.rs:7-25`, each in its own `systemd-run --user --scope`) and workers in Herdr panes.
  When it has none, `cgroup_used_bytes` is `None` and `outside_used_bytes` is the `rss_bytes` of every tracked job and worker.
  `cgroup_limit_bytes` is passed only while a new worker on this device would use the headless host of Task 8; until that task it is `None`.
- The numbers per profile are starting values and settings (A25):

  | Number | `laptop` | `workbox` | `production` |
  |---|---|---|---|
  | Reserve kept for the system or the services | 0 | 1024 MiB | 1536 MiB (`SERVER_RAM_RESERVE_MIB`, `guard_policy.rs:65`) |
  | Share idlemax may fill | none | none | the sized `ram_budget_mib`, a quarter of the memory |
  | Hold, shed, emergency at memory used | 90, 95, 98 percent (D14) | the same | the same, of the share |
  | Hold at memory pressure `some avg10` | 5 percent | 10 percent | 5 percent |
  | Shed at memory pressure `full avg10` | 1 percent | 5 percent | 1 percent |
  | Free disk for hold, shed, emergency | 4, 2, 1 GiB (D14) | the same | the same |

  `DispatcherConfig` gains the three optional keys `linux_reserve_mib`, `linux_psi_some_hold_pct` and `linux_psi_full_shed_pct`; an unset key takes the profile's value, `validate` refuses a percent outside 0 to 100, and like every `dispatcher.*` key they apply without a restart (A2).
- `WorkerRoom::evaluate` (`dispatch/room.rs`) gains one argument and one branch: `evaluate(&mut self, m: &MetricsSnapshot, linux: Option<(&LinuxFacts, &LinuxNumbers)>)`.
  With `None` every result is what Spec 1 gives, so the macOS path is untouched and its tests only pass the new argument.
  With `Some`, the memory line reads `linux_room`: an `Err` is `Hold` with that text as the reason, never `Shed` or `Emergency`, because stopping workers over a file that could not be read is the wrong failure.
  Otherwise `used_pct` is compared with the three `*_mem_used_pct` numbers, and the count of open slots uses the budget in place of the installed memory: `floor((hold_mem_used_pct - used_pct) / 100 * budget_mib / worker_mem_mib)`.
  The result is not written into `MetricsSnapshot.mem_used_pct`, which stays the Activity Monitor figure of the Mac and `None` on Linux; the Linux percent and budget appear in the room's `reason`.
- Nothing to shed is not an emergency: when the Linux memory line would read `Shed` or `Emergency` and `tracked` is 0, it reads `Hold` with the same figures and `nothing of idlemax's is running`.
  On a `production` host the services alone can take the available memory under the reserve, which makes the percent 100 by design; without this rule such a host would show a standing emergency, and Spec 4 puts every device in `emergency` on the "needs you" list.
- Pressure replaces the swap-in rule on Linux (D19 names pressure, available memory and the cgroup limit): with `Some`, `evaluate` skips the swap-in marks.
  Linux pages are a quarter of the size the swap-in numbers were chosen for, and Linux reads cold pages back in during normal work, which the guard preset already says of swap (`guard_policy.rs:145-146`).
  `some avg10` at or above its number is `Hold`, `full avg10` at or above its number is `Shed`, and pressure never raises an emergency, which D14 gives to memory and disk only.
  A sample without pressure figures is `Hold` with `memory pressure is not read on this system: boot the kernel with psi=1`.
- Disk, the battery floor and thermal throttling (A17) work on Linux as Spec 1 built them, and `evaluate` still never reads `user_idle_secs` (A16).
- Docs in this task: the README table of the lines gets the three profile columns and the three keys, and `docs/API.md` says what `room.reason` holds on Linux.

**Definition of Done:**

- [x] `workbox`, 16384 MiB total: 4096 MiB available gives 80.0, 2560 MiB gives 90.0, and 1024 MiB or less gives 100.0.
- [x] `production`, 16384 MiB total, a share of 4096 MiB with 3072 MiB in use, 8192 MiB available: `used_pct` is 75.0, `budget_mib` is 4096, and `evaluate` reads `run` with 1 open slot at `worker_mem_mib` of 512.
- [x] `production`, 3379 MiB total with 1200 MiB available: `used_pct` is 100.0, and `evaluate` reads `hold` with nothing tracked and `emergency` with one worker tracked.
- [x] A cgroup limit of 2048 MiB with 1024 MiB in use caps a `workbox` at 50.0 while the host has room, 512 MiB of tracked use outside the cgroup raises that to 75.0, and a limit with unknown use is an `Err` that names the cgroup.
- [x] `laptop` with a tenth of its memory available gives 90.0.
- [x] With the `workbox` numbers `evaluate` holds at `some avg10` 10.0 and sheds at `full avg10` 5.0, stays `run` at 9.9 and 4.9, holds when the sample has no pressure figure, and ignores a swap-in rate of 500 pages per second.
- [x] Every test of Spec 1 in `dispatch/room/tests.rs` passes with `None` as the new argument and no changed assertion.
- [x] `memory_used` reads a v2 and a v1 fixture directory and takes the use from the ancestor that holds the tightest limit.
- [x] Verify: `cargo test -p idlemax-core dispatch::linux && cargo test -p idlemax-core dispatch::room && cargo test -p idlemax-core config && cargo test -p idlemax --bin idlemax agent::cgroup && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 3: Switch devices on and off on the coordinator

**Objective:** Store one switch per device, off by default, and make it one gate that the Plan, the feed reply, the orchestrator pass, every dispatcher job and every launch respect.
Switching a device off stops its workers with their state saved and ends the dispatcher's own jobs there; switching it on lets the workers continue.

**Files:**

- Modify: `crates/idlemax/src/coord/store/schema.rs`
- Modify: `crates/idlemax/src/coord/store/devices.rs`
- Create: `crates/idlemax-core/src/control/device.rs`
- Modify: `crates/idlemax-core/src/control/mod.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Create: `crates/idlemax/src/coord/api/devices.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/coord/api/plan.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax-core/src/worker/control.rs`
- Modify: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `crates/idlemax/src/coord/api/runs.rs`
- Modify: `crates/idlemax-core/src/dispatch/plan.rs`
- Modify: `crates/idlemax-core/src/dispatch/orchestrator.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `crates/idlemax/src/coord/store/dispatch.rs`
- Modify: `crates/idlemax/tests/plan.rs`
- Modify: `docs/API.md`
- Test: `crates/idlemax-core/src/control/device.rs` (inline tests), `crates/idlemax/src/coord/store/devices.rs` (inline tests), `crates/idlemax/src/coord/api/devices.rs` (inline tests), `crates/idlemax-core/src/dispatch/plan/tests.rs`, `crates/idlemax-core/src/dispatch/orchestrator/tests.rs`, `crates/idlemax-core/src/dispatch/view.rs` (inline tests), `crates/idlemax/src/coord/api/plan.rs` (inline tests)

**Key Decisions / Notes:**

- The migration is the one migration of this whole plan, the next `Vn` in `schema.rs`.
  `MIGRATIONS` ends at `V7` today (`schema.rs:4`), Specs 1, 2 and 3 take `V8`, `V9` and `V10` and Spec 4 takes `V11` and `V12`, so `V13` is the number to expect; the implementer reads `MIGRATIONS` on `main` first and takes the next free one.

  ```sql
  -- The owner's switch per device; off until `idlemax device on` (D19).
  ALTER TABLE devices ADD COLUMN dispatch_on INTEGER NOT NULL DEFAULT 0;
  -- How the device's agent proved itself on its last heartbeat (Task 9); NULL before the first one.
  ALTER TABLE devices ADD COLUMN agent_auth TEXT;
  -- The limits each device reports, as JSON (Task 4); NULL from an agent that predates it.
  ALTER TABLE devices ADD COLUMN limits TEXT;
  -- One credential per device, kept as its SHA-256 digest (Task 9).
  CREATE TABLE device_credentials (
    device TEXT PRIMARY KEY,
    digest TEXT NOT NULL UNIQUE,
    created_ms INTEGER NOT NULL
  ) STRICT, WITHOUT ROWID;
  ```

  The default makes every known device and every new one off, the Mac included (D19, A23).
  `UPSERT_DEVICE` (`store/devices.rs:27-35`) does not name `dispatch_on`, so a heartbeat can never change it, and a device forgotten after 30 days of silence comes back off.
- `Store::set_device_dispatch(id, on)` returns the value before the change, or `None` for an unknown id, and `devices()` (`store/devices.rs:152-167`) fills the new `DeviceView.enabled: Option<bool>` (`protocol.rs:162-182`), left out by a coordinator that predates it.
  That field is what Spec 4's `DispatcherView.devices[].enabled` reads, and its "needs you" entry `device:<id>:off` for an off device with switched-on projects appears from it with no further code (A23).
- `control/device.rs` holds the pure part:

  ```rust
  pub enum Switch { Changed, Already }
  pub fn plan_switch(known: &[String], current: Option<bool>, id: &str, on: bool) -> Result<Switch, ControlError>;
  /// `device <id> is off: idlemax device on <id>`
  pub fn off_reason(id: &str) -> String;
  /// Why this device takes no dispatcher work whatever its room, or None.
  pub fn device_gate(view: &DeviceView) -> Option<String>;
  ```

  `plan_switch` returns the new `ControlError::UnknownDevice`, whose message lists the known ids, for a device that never sent a heartbeat.
  `device_gate` returns `off_reason` unless `enabled` is `Some(true)`; Task 9 adds its second rule.
- The routes are `POST /v1/devices/{id}/on` and `POST /v1/devices/{id}/off`, two calls (A3), in the classes Spec 4's access table already gives them: the `on` handler calls `CallerCheck::ensure(peer, Access::Owner)` and the `off` handler `ensure(peer, Access::Stop)`.
  They answer `{"device", "enabled", "changed", "runs_stopped", "jobs_stopped"}`.
  An unknown device is 404, which is a new status: a pause on an unknown device keeps its 400 (`coord/api/jobs.rs:193-205`); an id that fails `name_problem` (`protocol.rs:58-63`) is 400.
  A device id may hold `/` or a space, so every client puts it into the path percent-encoded.
  Both routes are owner actions and work whatever a pause, the kill switch, the guard or the line state says (A10), and each change appends one `EventKind::Control` event that names the device and its new state.
- One gate, asked in every place the dispatcher acts on a device (D19: nothing starts on an off device):
  1. `build` (`dispatch/plan.rs`) sets `DevicePlan.held` to `device_gate` before its own three reasons, so the Plan gives the device no pick, and `pace::may_launch`, which reads `held`, refuses a launch and a relaunch there.
  2. `pass_due` (`dispatch/orchestrator.rs`) counts a device as able to take picks or to run the pass only when `device_gate` is `None`, so no orchestrator pass is placed on an off device.
  3. The reply of `POST /v1/agent/dispatch-inputs` (`coord/api/plan.rs`) is empty for a gated device, as it is for a paused one, so its agent prepares no base checkout and reads no project facts.
  4. `step` (`crates/idlemax/src/dispatch/mod.rs`) queues no setup job, no check job and no pass for a project whose device is gated.
  5. `lease_lines` (`coord/api/agents.rs`) leases no dispatcher job to a gated device, so a job queued before the switch cannot start after it.
- Switching off does three things in one `app.with_store` call (A11, A23).
  It stores the switch.
  It stops the device's Runs in `starting`, `running` and `checking` with their state saved, through `dispatch::stop_workers` with the device scope this task adds: `control::plan_stop(runs, scope)` gives `shed` for a device scope, as it does for an account scope, and each Run's job is marked `killed`.
  It ends every other job of that device for which `is_dispatcher_job` is true: a queued one is cancelled and an active one killed, through `apply_control` (`coord/api/jobs.rs:180-219`) with `stop_all` on the job scope, which stores no pause.
  The Runs are `shed`, so they continue by themselves once the device is on again and the lines and the Budget allow it; no owner stop row is written, because switching the device back on is the owner's own act.
- Switching on stores the switch and deletes the `base_checks` row of each of that device's projects whose check ended without a result, with the new `Store::clear_base_check(project)` in `coord/store/dispatch.rs`, so `next_step` sees no record and `step` starts those checks again.
  If Spec 1's merged code already has the delete it uses when a project is switched off, this task calls that one and adds no second method.
- `DevicePlan` (Spec 1's Plan) gains `enabled: Option<bool>`, copied from `DeviceView.enabled`, so that `dispatcher_view` can fill Spec 4's `DeviceLine.enabled`; Spec 4's view tests and the TUI fixtures expect the field.
- `POST /v1/workers/stop` with a `device` scope keeps answering 400, now with the text `use POST /v1/devices/<id>/off`: a stop without the switch would be undone by the next sweep, which relaunches `shed` runs.
- Every device is off after this task, so the tests of Specs 1 to 4 that expect a pick, a pass, a dispatcher job or a launch switch their device on first.
  Pure tests set `enabled: Some(true)` on their `DeviceView` fixtures (`dispatch/plan/tests.rs`, `dispatch/orchestrator/tests.rs`, `dispatch/view.rs`, `worker/pace.rs`).
  Store-backed tests call `set_device_dispatch` after their first heartbeat (`coord/api/plan.rs`, `coord/api/pass.rs`, `coord/api/worker_tests.rs`, `coord/dispatch.rs`, `coord/api/budget/tests.rs`).
  The binary test `crates/idlemax/tests/plan.rs` posts `/v1/devices/<id>/on` with its HTTP client before it reads the plan.
  No assertion of those tests changes, and the tests of plain jobs (`tests/e2e.rs`, `scheduler/tests.rs`, `reconcile/tests.rs`) need no switch.

**Definition of Done:**

- [x] A fresh store and a store migrated with one device row both report that device with `enabled` false, and a heartbeat leaves a switched-on device on.
- [x] `POST /v1/devices/mac/on` answers `changed` true the first time and false the second, and `GET /v1/status` then shows `enabled` true for it.
- [x] `POST /v1/devices/nope/on` answers 404 with a message that lists the known device ids, and a device id that holds a space is switched through its percent-encoded path.
- [x] With a caller lookup that returns a chain marked for this home, `on` answers 403 and `off` answers 200.
- [x] For an off device with one project on: the Plan has no pick and gives the `off_reason` text as `held`, `may_launch` refuses, a `shed` run of that device is not relaunched, `pass_due` refuses, the dispatch-inputs reply is empty, `step` queues no job, and `lease_lines` leaves a queued `dispatch-check` job unleased.
- [x] Switching off a device with two running workers and one running check job leaves both Runs `shed`, all three jobs `killed`, reports `runs_stopped` 2 and `jobs_stopped` 1 and stores no owner stop row and no pause; a plain job leased to that device is untouched.
- [x] After `on`, `launch_due` relaunches the two Runs one at a time, and `step` queues the check again.
- [x] `dispatcher_view` over an off device with one project on holds the "needs you" entry `device:<id>:off`.
- [x] The dispatcher tests of Specs 1 to 4 pass with their device switched on and no changed assertion, and `cargo test -p idlemax --test e2e` passes with no device switched on.
- [x] `docs/API.md` lists both routes, their replies and `DeviceView.enabled`.
- [ ] Verify: `cargo test -p idlemax-core control::device && cargo test -p idlemax-core worker::control && cargo test -p idlemax-core worker::pace && cargo test -p idlemax-core dispatch::plan && cargo test -p idlemax-core dispatch::orchestrator && cargo test -p idlemax-core dispatch::view && cargo test -p idlemax --bin idlemax coord::store::devices && cargo test -p idlemax --bin idlemax coord::api::devices && cargo test -p idlemax --bin idlemax coord::dispatch && cargo test -p idlemax --bin idlemax plan && cargo test -p idlemax --bin idlemax coord::api::pass && cargo test -p idlemax --bin idlemax worker_tests && cargo test -p idlemax --bin idlemax coord::api::budget && cargo test -p idlemax --test plan --test e2e && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 4: Add idlemax device on and off, the device toggle and the read-only limits

**Objective:** Give the owner the two commands that flip a device, make the device toggle of the dashboard and the TUI call the same two routes, and show each other device's limits read-only (A23).

**Files:**

- Create: `crates/idlemax/src/cli/device.rs` (with `idlemax device list`, added by the orchestrator's brief beside `on` and `off`)
- Modify: `crates/idlemax/src/cli.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/client.rs`
- Modify: `crates/idlemax/src/cli/home/render.rs`
- Modify: `skills/idlemax/SKILL.md`
- Modify: `crates/idlemax/assets/dashboard.js`
- Modify: `crates/idlemax/src/tui/dispatcher.rs`
- Modify: `crates/idlemax/src/tui/dispatcher_view.rs`
- Modify: `crates/idlemax/src/tui/feed.rs`
- Modify: `crates/idlemax/src/tui/app/keys.rs`
- Modify: `crates/idlemax/src/tui/snapshots/dispatch-80x24.txt`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Modify: `crates/idlemax-core/src/dispatch/view.rs`
- Modify: `crates/idlemax/src/agent/event_loop.rs`
- Modify: `crates/idlemax/src/agent/event_loop/coord.rs`
- Modify: `crates/idlemax/src/coord/store/devices.rs`
- Modify: `crates/idlemax/src/coord/api/dispatcher_view.rs`
- Modify: `README.md`, `docs/API.md`
- Create: `crates/idlemax/tests/devices.rs`
- Test: `crates/idlemax/tests/devices.rs`, `crates/idlemax/src/tui/app/tests.rs`, `crates/idlemax-core/src/dispatch/view.rs` (inline tests)

**Key Decisions / Notes:**

- `Cmd::Device` with `On { id }` and `Off { id }` joins `cli/commands.rs` (`:12-95`), and `dispatch` (`cli/dispatch.rs:145-177`) hands it to `device::run`, which makes one client call and renders through `out.emit_with`, as `handlers::control` does (`cli/handlers.rs:55-86`), so `--json` and the agent output work as for every other command (D19, the shared command names).
  The client call pushes the id as one path segment, so it is percent-encoded.
- `access_of` (`cli/dispatch.rs`, Spec 4) classes `device on` as `Owner` and `device off` as `Stop`; its match has no wildcard arm, so the build fails until both are classed, and `execute` then refuses `idlemax device on` when its own environment holds `LAUNCHED_ENV`, before any call.
- The reply says what changed and the next step: after `on`, `idlemax project list --device <id>`; after `off`, how many workers and dispatcher jobs were stopped and that `idlemax device on <id>` continues the workers.
  A switch that changes nothing prints `already on (no-op)` or `already off (no-op)` and exits 0, like the other no-ops (`cli/noop.rs`).
  An unknown id exits non-zero, lists the known devices and names `idlemax status`.
- Choosing projects per device (D19) needs no code here: Spec 1's `idlemax project list`, `on`, `off` and `set` take `--device <id>`, fail as a usage error that lists the devices when a name exists on two, and `GET /v1/projects?device=<id>` keeps one device.
  A project may be switched on while its device is off; the Plan still offers nothing there until the device is on, since both fences must be open (D5, D19).
- The device toggle (D12): Spec 4 draws it from `DispatcherView.devices[].enabled` and names no call.
  In the dashboard it goes through `act()` to `POST /v1/devices/<id>/on` or `/off` with the id passed through `encodeURIComponent`; `on` asks first through `confirmBox()` and `off` does not, like the project toggle.
  Spec 4 hides the toggle while the view has one device; this task draws it whenever `enabled` is set, because after this plan a lone Mac is off and needs its switch.
  In the TUI the Dispatch tab gains a `devices` pane after `projects` (device, on or off, line state, reason), `t` switches the selected device as it switches the selected project, toward on it asks through the confirm box, and `Request` (`tui/feed.rs`) gains `DeviceSwitch { id, on }` with one client call in `perform`.
- The limits of other devices (A23): a device's lines are read from its own `config.toml`, and `POST /v1/limits` edits only the file of the coordinator's machine.
  Each agent sends `Heartbeat.limits: Option<Vec<Setting>>` (`protocol.rs:30-45`), the list `limit_settings` gives for its own `config.toml`, rebuilt when its `LiveConfig` sees a new file; the store keeps it in `devices.limits` and `DeviceView.limits` carries it.
  `DispatcherView` gains `limits_device: Option<String>`, the device id of the coordinator's own machine (`agent.device_id` of its config, else `default_device_id` of its host name, `agent/metrics/mod.rs:152`), and `dispatcher_view` fills `devices[].limits` for every other device from what it reports.
  The Limits card and the limits pane name `limits_device` in their heading, so the owner sees whose file `POST /v1/limits` changes, and draw the other devices' lists read-only, as Spec 4 already does for a list that is not empty.
- The command list in `cli/home/render.rs` (`:18-57`) gains `("idlemax device on <id>", "let the dispatcher use a device; every device starts off")`, and `skills/idlemax/SKILL.md` is regenerated with `cargo run -p idlemax -- skill > skills/idlemax/SKILL.md`.
- `tests/devices.rs` starts a coordinator and an agent in a temp home on spare ports and asserts the home is not the real one, as `tests/e2e.rs` does (`assert_isolated`, `tests/e2e.rs:75-86`); its agent config sets `advisor_inputs_secs = 0` and `dispatch_inputs_secs = 0`.
- Docs in this task: the README "Controls" section gains the device switch, "Deploying across devices" and the upgrade notes say every device starts off and name `idlemax device on <id>`, "Change a setting" says whose limits the dashboard changes, and `docs/API.md` describes `Heartbeat.limits` and `DispatcherView.limits_device`.

**Definition of Done:**

- [x] In a temp home, `idlemax device on <id> --json` exits 0 with `enabled` true and `changed` true, a second run reports the no-op, and `idlemax status --json` shows the device enabled.
- [x] `idlemax device off nope` exits non-zero, and its message lists the known device ids and names `idlemax status`.
- [x] With `IDLEMAX_JOB_ID` in its environment and `--coord` at a closed port, `idlemax device on <id>` ends with the lock's text and `idlemax device off <id>` ends with a connection error.
- [x] In the TUI, `t` then `y` on a device that is off yields one `DeviceSwitch` request with `on` true, `t` on a device that is on yields one with `on` false and no question, and the regenerated `dispatch-80x24.txt` holds the `devices` pane.
- [ ] The dashboard script calls `/v1/devices/` with the encoded id for both directions, draws the toggle for a view with one device, and the dashboard test still finds one script tag and no HTML sink.
- [x] `dispatcher_view` over two devices gives the reported limits to the one that is not `limits_device` and an empty list to the one that is.
- [x] `idlemax skill --check` passes and the skill lists `idlemax device on <id>`.
- [ ] Verify: `cargo test -p idlemax --test devices && cargo test -p idlemax-core dispatch::view && cargo test -p idlemax --bin idlemax tui:: && cargo test -p idlemax --bin idlemax the_dashboard_runs_one_same_origin_script && cargo test -p idlemax --test skill && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 5: Read each device's Claude login and check the logins in doctor

**Objective:** Let each agent report which Claude login it holds, as an id and never as an email address or a token, and let `idlemax doctor` say "needs you" for an agent CLI that an account needs and that is not logged in.
Where idlemax cannot tell, it says so, keeps the last good reading for a day and guesses nothing.

**Files:**

- Create: `crates/idlemax-core/src/quota/login.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax-core/src/types/device.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Create: `crates/idlemax/src/agent/metrics/login.rs`
- Modify: `crates/idlemax/src/agent/metrics/mod.rs`
- Modify: `crates/idlemax/src/agent/probes.rs`
- Create: `crates/idlemax/src/doctor/logins.rs`
- Modify: `crates/idlemax/src/doctor/mod.rs`
- Modify: `crates/idlemax/src/doctor/facts.rs`
- Modify: `crates/idlemax/tests/doctor.rs`
- Modify: `README.md`, `docs/API.md`
- Test: `crates/idlemax-core/src/quota/login.rs` (inline tests), `crates/idlemax/src/agent/metrics/login.rs` (inline tests), `crates/idlemax/src/doctor/logins.rs` (inline tests), `crates/idlemax/tests/doctor.rs`

**Key Decisions / Notes:**

- `quota/login.rs` is pure and reads the text a status command printed:

  ```rust
  #[serde(tag = "state", rename_all = "snake_case")]
  pub enum Login { In { id: Option<String> }, Out, Unknown { reason: String } }
  pub const LOGIN_KEEP_MS: i64 = 86_400_000;
  pub fn claude_login(stdout: &str) -> Login;
  pub fn codex_login(exit_code: Option<i32>, stdout: &str) -> Login;
  /// What to report after a new reading: a good earlier one stands in for an `Unknown` one for a day.
  pub fn keep_last_good(last_good: Option<(&Login, UnixMs)>, new: Login, now_ms: UnixMs) -> Login;
  ```

- Claude (D19): the reply of `claude auth status --json` was read on 2026-11-12 and has the keys `loggedIn`, `authMethod`, `apiProvider`, `analyticsDisabled`, `projectsDirectory`, `configDirectory`, `loginExpiresAt`, `email`, `orgId`, `orgName`, `subscriptionType`, `storedClaudeAiLogin` and `planTier`.
  `claude_login` parses into a type with two fields, `loggedIn` and `orgId`, so the email address and every other key are dropped by the parser, never logged and never stored.
  `loggedIn` true gives `In` with the `orgId`, `loggedIn` false gives `Out` (seen with an empty config directory, exit code 1, no `orgId` key), and text that is not this JSON gives `Unknown` with the reason.
- Codex: `codex login status` only says that a login exists (`docs/DISPATCHER.md`, measurements).
  Exit code 0 gives `In` with no id; the text `Not logged in` with exit code 1, seen with an empty `CODEX_HOME`, gives `Out`; every other outcome gives `Unknown`.
- The agent sends three states, not an optional id: `DeviceInfo.claude_login: Option<Login>` (`types/device.rs:85-103`), where `None` means only that the agent did not ask.
  A logged-out CLI is `Out`; a timeout, output that is not the JSON and a failed start are `Unknown` with the reason; so the coordinator never reads "could not tell" as "logged out".
- When the agent asks: only when `claude` is among the detected tools and its own `config.toml` holds at least one `[[quota.accounts]]` entry whose provider is `claude`, whether or not `agent.ai_accounts` lists it.
  `Probes` already holds that config (`agent/probes.rs:34-52`).
  No dev or test run configures an account (`AGENTS.md`), so the suites that start an agent with the developer's `PATH` (`tests/e2e.rs:88-104`) never run the real `claude`.
  The command runs with stdin closed and a limit of 5 s through `metrics::cmd::run` (`agent/metrics/cmd.rs:17-50`), and the agent never opens a credential file or a keychain (D19: "never reads a login token").
  The reader in `agent/metrics/login.rs` takes the `PATH` value as an argument, as `which_in` does (`doctor/facts.rs:42-49`), so its tests pass a temp directory with a stub and never change the environment of the test process.
- `Sampler::device_info` (`agent/metrics/mod.rs:126-147`) keeps the last `In` reading with its time and reports `keep_last_good` of it and the new reading, so one slow status call on a full workbox, where the agent runs at `Nice=19`, does not close the account until the next hourly refresh (`event_loop/sample.rs:49-63`); after `LOGIN_KEEP_MS` an `Unknown` reading is reported as it is.
  `Heartbeat::check` (`protocol.rs:65-98`) bounds the id at 128 characters and the reason at 200 with `name_problem`, and an older agent simply sends no field.
- `doctor/logins.rs` adds the login checks after the AI account checks (`doctor/mod.rs:36`), one per agent CLI that an account of this device's config needs; `CmdOutcome::Ran` (`doctor/facts.rs:8-18`) gains `stdout` for it.
  `In` is `ok` and shows the first 8 characters of the id.
  `Out` is a `warn` whose detail starts with `needs you:` and says no worker can use that CLI on this device, with the fix `claude auth login` or `codex login`, run as the user the agent runs as (D19: a missing login is "needs you").
  `Unknown` is a `warn` whose detail starts with `cannot tell whether` and whose fix is to run the status command by hand.
  A CLI that is not installed is a `warn` that starts with `needs you:` and names the program.
  An account whose CLI has no known status command (`agy`, `opencode`) gets an `ok` line that says idlemax cannot check its login, which is a statement and not a guess.
  A config with no account adds no login check and runs no command.
- Docs in this task: the README list of doctor checks gains the login checks, and `docs/API.md` describes `DeviceInfo.claude_login` and its three states.

**Definition of Done:**

- [x] `claude_login` returns `In` with the `orgId` for a logged-in reply, `In` with no id for a logged-in reply without `orgId`, `Out` for `{"loggedIn": false}` and `Unknown` for text that is not JSON.
- [x] A reply that holds an `email` key gives a result whose debug text does not contain that value.
- [x] `codex_login` returns `In` for exit code 0, `Out` for `Not logged in` with exit code 1, and `Unknown` for exit code 2 or a timeout.
- [x] `keep_last_good` returns the earlier `In` for an `Unknown` reading 23 hours later, the `Unknown` reading 25 hours later, and always the new reading when it is `In` or `Out`.
- [x] With a stub `claude` that writes a marker file and a config with no account, the device info has no `claude_login` and the marker file does not exist; with one `claude` account in the config and a stub that reports an id, the device info carries `In` with that id.
- [x] In `tests/doctor.rs`, with one `claude` account in the temp config and a stub `claude` first on `PATH` that prints `{"loggedIn": false}`, `idlemax doctor --json` holds a check `login.claude` at level `warn` whose detail starts with `needs you:`; a stub that prints other text gives a detail that starts with `cannot tell whether`; with no account the report holds no `login.` check.
- [x] Verify: `cargo test -p idlemax-core quota::login && cargo test -p idlemax --bin idlemax doctor::logins && cargo test -p idlemax --bin idlemax agent::metrics && cargo test -p idlemax --test doctor && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 6: Keep one Budget per login and pace a server-only account

**Objective:** Make an account name mean one login on every device, so that two devices on the same login share one Budget and different logins never do.
Tell Spec 3's Budget when an account is used only by servers, so the owner's reserved share drops to a small margin.

**Files:**

- Create: `crates/idlemax-core/src/quota/identity.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax-core/src/quota/config.rs`
- Modify: `crates/idlemax-core/src/report/quota.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Modify: `crates/idlemax-core/src/dispatch/view.rs`
- Modify: `crates/idlemax/src/coord/api/budget.rs`
- Modify: `crates/idlemax/src/coord/api/jobs.rs`
- Modify: `crates/idlemax/src/coord/api/dispatcher_view.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `crates/idlemax/src/cli/print.rs`
- Modify: `README.md`, `docs/API.md`, `examples/config.toml`
- Test: `crates/idlemax-core/src/quota/identity.rs` (inline tests), `crates/idlemax/src/coord/api/budget/tests.rs`

**Key Decisions / Notes:**

- A Budget is kept per account name (Spec 3), so "the same login shares one budget, different logins have separate ones" (D19) holds exactly when one name means one login and one login has one name.
  `identity.rs` checks that from the device rows the coordinator already holds and closes what breaks it:

  ```rust
  pub struct DeviceLogins<'a> { pub device: &'a str, pub profile: DeviceProfile,
                                pub accounts: &'a [String], pub has_claude: bool,
                                pub claude_login: Option<&'a Login> }
  pub enum ProblemKind { NotLoggedIn, LoginUnknown, SplitName, OneLoginTwoNames, SameReading, OwnerMayUse }
  pub struct AccountProblem { pub kind: ProblemKind, pub accounts: Vec<String>, pub devices: Vec<String>,
                              pub message: String, pub fix: String }
  pub fn account_problems(cfg: &QuotaConfig, devices: &[DeviceLogins<'_>],
                          weekly: &[QuotaSnapshot], now_ms: UnixMs) -> Vec<AccountProblem>;
  pub fn usable_on(problems: &[AccountProblem], device: &str, account: &str) -> Result<(), String>;
  pub fn server_only(cfg: &QuotaConfig, account: &str, devices: &[DeviceLogins<'_>]) -> bool;
  ```

  `has_claude` is whether `claude` is among the device's `capabilities`.
- The four closing rules, for accounts whose provider is `claude`:
  `NotLoggedIn` closes the account on a device that lists it and reports `Out`.
  `LoginUnknown` closes it on a device that lists it and reports `Unknown`, `In` without an id or no login field at all, which is also an agent older than Task 5; its message starts with `cannot tell whether` and its fix is `idlemax doctor` on that device.
  `SplitName` closes the account on every device when two devices report different login ids under one name; the fix is to give one login its own `[[quota.accounts]]` entry and list that name in the device's `agent.ai_accounts`.
  `OneLoginTwoNames` closes every name but the first in `[[quota.accounts]]` order when two names resolve to one login id, because two budgets would spend one quota twice; the fix is one name on every device.
- For Codex the name the owner gives once per device is the account name that device lists in `agent.ai_accounts` (D19), and no rule closes a Codex account.
  `SameReading` is a warning: two `codex` accounts with different names whose newest weekly snapshots are both under `SNAPSHOT_MAX_AGE_MS` old (`quota/rules.rs:12`), reset within `SAME_RESET_MS` of each other (`report/quota.rs:15-16`, made visible to the crate) and read within 1.0 point of each other (A25).
  Its message says the two may be one login and that one name on both devices then shares the budget.
- `usable_on` is one more gate and never a bypass, asked in two places.
  `gather_plan` (`crates/idlemax/src/dispatch/mod.rs`) takes every device and account pair it refuses out of that device's `ai_accounts` before it calls `build` and `pass_due`, which both use an account only on a device that lists it, and adds one line per pair to `Plan.notes` with the message and the fix.
  `launch_due` (`coord/dispatch.rs`) asks it before a launch and before it relaunches a `shed` run, and records a refusal like the other refusals.
  Old AI jobs keep the device rule they have (A8).
- `ClusterStatus` (`protocol.rs:215-231`) gains `account_problems`, filled in `cluster_status` (`coord/api/jobs.rs:221-255`) from the same device rows, left out by an older coordinator.
  `idlemax quota` prints each problem with its fix under the table.
  `ViewInput` (`dispatch/view.rs`) gains the list, and `dispatcher_view` adds each closing problem to `needs_you` as an entry of kind `account` with the key `account:<name>:<kind>`.
- The server-only account (D19).
  For every provider, `server_only` needs at least one known device that lists the account and no `laptop` among them; a laptop that is asleep still counts, because the coordinator keeps its row for 30 days.
  For a `claude` account it also needs that no `laptop` is, or may be, on that login: a laptop that reports `In` with the id a listing device reports makes it false, and so does a laptop with `has_claude` that reports `Unknown`, `In` without an id or nothing.
  A laptop that reports `Out` or another id, or has no `claude`, does not count.
  So a laptop where the owner uses the login without listing the account for idlemax keeps the owner's full reserve.
  When the unknown login of a laptop is the only thing that keeps an account from being server-only, `account_problems` adds the warning `OwnerMayUse`, which closes nothing, names that laptop and gives the fix: add a `[[quota.accounts]]` entry for the laptop's own Claude account to its `config.toml`, so its agent reports the login.
  Codex has no such check, because its status command names no login; the README says to list a Codex account on every device where that login is used.
- The new setting `quota.server_only_margin_pct` (5.0, A25) sits beside Spec 3's percent keys in `QuotaConfig` (`quota/config.rs`), with their range check and their live reload (A2).
- What is passed to Spec 3, and nothing else: `budgets` (`coord/api/budget.rs`) reads the device rows from the store and sets `BudgetInput.owner_reserve_override_pct` to `Some(server_only_margin_pct)` for a server-only account and to `None` otherwise.
  Spec 3's `budget` then holds only that margin for the owner, learns nothing about the owner's use, and its pacing by the days left to the reset spreads the rest evenly over the week; its safety margin, 5-hour stop (A18), last-day floor and measuring-week cap still apply (Open Question 5).

**Definition of Done:**

- [x] Two devices that list `claude-a` and report the same login id give no problem, and `usable_on` allows both.
- [x] Two different login ids under `claude-a` give one `SplitName` that names both devices, and `usable_on` refuses `claude-a` on both with the fix in its text.
- [x] One login id under `claude-a` and `claude-b`, in that config order, leaves `claude-a` usable and refuses `claude-b`.
- [x] A device that lists `claude-a` and reports `Out` gives `NotLoggedIn` for that device only, and one that reports `Unknown` gives `LoginUnknown` with a message that starts with `cannot tell whether`.
- [x] Two `codex` accounts with fresh weekly readings of 41.0 and 41.5 that reset 2 minutes apart give one `SameReading` and stay usable; readings of 41.0 and 47.0 give none.
- [x] `server_only` is true for a `claude` account listed by two `workbox` devices while a `laptop` reports another login id, false once the `laptop` lists it, false when the `laptop` reports the same id without listing it, false with one `OwnerMayUse` warning when the `laptop` has `claude` and reports nothing, and false for an account nobody lists.
- [x] `server_only` is true for a `codex` account listed only by `workbox` devices whatever a `laptop` reports.
- [x] For an account listed only by `workbox` devices, the Budget from `budgets` equals the one `budget` gives with an override of 5.0, and for an account a `laptop` lists it equals the one with no override.
- [x] With `claude-a` closed on one of two devices, the Plan gives that device no pick on `claude-a`, holds a note with the fix, and `launch_due` does not relaunch a `shed` run of that pair.
- [x] `GET /v1/status` carries `account_problems`, `idlemax quota` prints each with its fix, `dispatcher_view` lists a `SplitName` under `needs_you`, and the README and `docs/API.md` describe the rules and the new setting.
- [ ] Verify: `cargo test -p idlemax-core quota::identity && cargo test -p idlemax-core quota::config && cargo test -p idlemax-core dispatch::view && cargo test -p idlemax --bin idlemax coord::api::budget && cargo test -p idlemax --bin idlemax coord::dispatch && cargo test -p idlemax --bin idlemax plan && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 7: Run the base checks again when the day's budget is spent

**Objective:** When a device has no quota left for today, let it run the focused checks of its switched-on projects again, which cost no quota.
A failing check becomes a Task of source `broken_check` through Spec 1's own check job and `discover`, so the next day starts with a list of what is broken.

**Files:**

- Create: `crates/idlemax-core/src/worker/free_checks.rs`
- Modify: `crates/idlemax-core/src/worker/mod.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `README.md`, `docs/API.md`
- Test: `crates/idlemax-core/src/worker/free_checks.rs` (inline tests), `crates/idlemax/src/coord/api/plan.rs` (the coordinator tests of `step`)

**Key Decisions / Notes:**

- There is one check-job mechanism, Spec 1's (A11): `check_job` builds a `dispatch-setup` or `dispatch-check` job with `cost = cpu_heavy` and `admission = lines` in the checkout `$IDLEMAX_HOME/checks/<project id>`, `next_step` decides the next step from the project's `base_checks` record, `step` drives it, and `discover` makes the `broken_check` Task from a record with `passed = false`.
  Spec 1 runs that check once per base commit; this task only decides when it runs again on the same commit, and builds no job, no worktree code and no result path of its own.
- `free_checks.rs` is the pure "when" (D19: "out of quota does not mean idle"):

  ```rust
  pub const CHECK_EVERY_MS: i64 = 86_400_000;
  pub fn budget_spent(budgets: &[&Budget]) -> bool;
  pub struct CheckState { pub project: i64, pub last_ms: Option<UnixMs>, pub active: bool }
  pub fn checks_due(now_ms: UnixMs, allowed: bool, spent: bool, projects: &[CheckState]) -> Vec<i64>;
  ```

  `budget_spent` takes the Budgets of the accounts a device lists and is true when none is usable and at least one has the basis `spent` or `five_hour_stop`.
  `checks_due` returns each switched-on project of the device whose check is not active and whose `last_ms` is more than `CHECK_EVERY_MS` ago, once a day being enough for "the next day's task list" (A25).
  `last_ms` is `BaseCheck.finished_ms`, and `active` is true while the record has a job that has not ended.
  `allowed` is true only when the mode is `auto`, no owner stop is stored, `device_gate` of Task 3 is `None`, the device is online and not paused, and its profile takes `cpu_heavy` jobs.
- `step` (`crates/idlemax/src/dispatch/mod.rs`) calls `budgets`, `budget_spent` and `checks_due` per device and, for each project that is due, deletes its `base_checks` row with `Store::clear_base_check(project)`, the delete Task 3 uses when a device is switched on.
  `next_step` then sees no record and returns `Queue(..)` again, and the new job is not refused as a duplicate, because `Store::insert_job_once` looks only at unfinished jobs of that key.
- The check job stays a dispatcher job: it is admitted by the lines and not by the guard (A16), holds on a low battery and on heat (A17), counts as the one dispatcher job of its device, stops with the kill switch, every pause, `stop-all` and the device switch, and never runs in the owner's folder (D6).
  With the workers stopped by the spent budget, the room has the open slot the lines ask for.
- A `production` device runs no base check at all: `cost_allowed(Production, CpuHeavy)` is false (`scheduler/blockers.rs:89-95`), so a check job leased by device would wait for ever.
  `step` therefore queues no setup and no check job for a project whose device's profile refuses `cpu_heavy`, and the Plan carries the note `<project>: base checks do not run on a production device`.
  Spec 2's launch records the baseline itself when the spec carries none, so a worker there is still checked against its base.
- Docs in this task: the README dispatcher section and `docs/API.md` say when the checks run again and that they cost no quota.

**Definition of Done:**

- [x] `budget_spent` is true for accounts with the bases `spent` and `five_hour_stop`, false when one account is usable, false for no accounts and false for one `paused` account.
- [x] `checks_due` returns nothing when `allowed` or `spent` is false, for a project checked 23 hours ago and for one with an active check, and returns a project checked 25 hours ago.
- [x] A coordinator test with a `workbox` that is on, mode `auto`, one account with the basis `spent` and a check that passed 25 hours ago runs `step` and finds one new queued `dispatch-check` job for the same base commit; a second `step` queues nothing more.
- [ ] The same test queues nothing in mode `propose`, for an off device, while a global pause holds the device, and for a check that passed 23 hours ago.
- [x] After the new check job is reported as failed with exit code 1, `discover` returns one Task with the source `broken_check` for that project.
- [ ] For a `production` device with one project on, `step` queues no `dispatch-setup` and no `dispatch-check` job and the Plan holds the note.
- [x] The README and `docs/API.md` say when checks run again and that they cost no quota.
- [x] Verify: `cargo test -p idlemax-core worker::free_checks && cargo test -p idlemax --bin idlemax plan && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 8: Add the headless runner

**Objective:** Build the second implementation of Spec 2's runner seam for a server without Herdr: the same brief, checks, stop and resume, with the worker's output in the job log that `idlemax logs` prints.
A server with a running Herdr keeps using the Herdr host of Spec 2 in its own local session.

**Files:**

- Create: `crates/idlemax/src/agent/worker/headless.rs`
- Modify: `crates/idlemax/src/agent/worker/mod.rs`
- Modify: `crates/idlemax/src/agent/worker/host.rs`
- Modify: `crates/idlemax/src/agent/worker/steps.rs`
- Modify: `crates/idlemax/src/agent/worker/record.rs`
- Modify: `crates/idlemax-core/src/worker/launch.rs`
- Modify: `crates/idlemax-core/src/config/workers.rs`
- Modify: `crates/idlemax-core/src/config.rs`
- Modify: `crates/idlemax/src/agent/event_loop/sample.rs`
- Modify: `docs/WORKER-PROOF.md`
- Modify: `README.md`
- Test: `crates/idlemax/src/agent/worker/headless.rs` (inline tests, unix only), `crates/idlemax-core/src/worker/launch.rs` (inline tests), `crates/idlemax/src/agent/worker/record.rs` (inline tests)

**Key Decisions / Notes:**

- `HeadlessHost` implements `WorkerHost` (`agent/worker/host.rs`: `start`, `state`, `send`, `stop`, `end_text`), so the worker manager, the checks, the verdict, stop and resume of Spec 2 run unchanged on a server without Herdr (D19).
- `start` spawns the agent CLI as a child of the agent in its own process group (as `runner/launch.rs:144-149`), in the worktree, with the minimal environment of `job_run::inherits` plus the pairs of Spec 2's `pane_env`, so the push block, the blanked credential names and the marker `IDLEMAX_JOB_ID` are the same as in a pane (D22, A12).
  Standard output and error go to the worker job's log, `Paths::job_log_file` (`paths.rs:74-76`), with the header and the footer the runner writes (`runner/launch.rs:201-226`, `runner/finish.rs:92`, `job_run/log.rs:3-4`).
  `idlemax logs <job>` and `--follow` then read a worker's output on its device with no change (`cli/dispatch.rs:70-79`), and on another device the command already says where to run it (`cli/logs.rs:112-144`).
- `send` writes the text to the child's standard input and closes it, because print mode reads its prompt from there to the end (the AI pass does the same, `advisor/claude.rs:48`).
  The order of Spec 2 holds: start, read the child's environment with `ProcTable::environ` and run `env_problems`, write the brief file, send the one line; a second `send` on one handle is a typed error.
- `state` reports `idle` before the send, `working` while the child lives after it and `done` once it has exited; the session id is the one passed at start, the shell pid is the child's pid, and it never reports `blocked`, since nothing can wait for input.
  `HostState` gains `exit_code: Option<i32>`, which the Herdr host leaves `None`.
  The manager (`agent/worker/steps.rs`) treats `done` with a code other than 0 as a turn that ended with an error: it takes `end_text` and ends the job `failed` with `the agent exited with code <n>`.
  `end_text` is the last 40 lines of the job log, cut to 2,000 characters, the same bound as the Herdr host, so the limit message of a print-mode run reaches Spec 3 (A9).
  `stop` sends SIGTERM to the process group, and the manager's search by pid and attempt tag kills what is left after 5 s, as for a pane (D11).
- `headless_args(cli, model, session)` in `worker/launch.rs` is pure and returns the arguments of Spec 2's `agent_args` with `-p` in front.
  Claude starts with `-p --model <model> --strict-mcp-config --permission-mode bypassPermissions --session-id <uuid>` and continues with `-p --model <model> --strict-mcp-config --permission-mode bypassPermissions --resume <session>`; each flag was read from `claude --help` on 2026-11-12.
  The session id is the uuid Spec 2's launch makes before the start, so a stop one second after the start can still be continued.
  `--strict-mcp-config` keeps the owner's personal MCP servers out, and nothing switches the project's own instructions off (D10 item 9).
  Codex, OpenCode and agy return the typed `Unsupported` error; for Codex it says that how `codex exec` reports the session id of a new run is not recorded (Open Question 4).
- Which host a launch uses is decided per launch by the pure `pick_host(setting, profile, herdr)` in `worker/launch.rs`, from the new key `workers.host` with the values `auto` (the default), `herdr` and `headless`, read through `LiveConfig` at each worker start (A2).
  A `production` device always gets the headless host, whatever the key says (A23): a pane runs outside the agent's cgroup, so the capped share could not be enforced there.
  `headless` gives the headless host and `herdr` the Herdr host, which fails closed when Herdr does not answer, as Spec 2 built it.
  `auto` gives the Herdr host when `herdr` is on the agent's `PATH` and answers `herdr workspace list` within Spec 2's 10 s limit, and the headless host otherwise, so a work server whose Herdr session is not running after a reboot still works around the clock; the worker job's first update says `headless: herdr did not answer`.
  `Config::validate` (`config.rs:160`) refuses `workers.host = "herdr"` beside an explicit `agent.profile = "production"`, so `idlemax config set workers.host herdr` fails with the fix instead of storing a value that does nothing.
  `plan_reload` puts `workers.host` under `applied`, like every `workers.*` key of Spec 2.
- `Workers` owns both hosts, and the saved worker record (`agent/worker/record.rs`) gains `host`, so a stop, a kill and `Workers::recover` go to the host that started the worker; a record without the field is a Herdr one.
  A headless worker is a child inside the agent's unit, whose `KillMode=mixed` (`packaging/linux/idlemax-agent.user.service:18`) ends it with the agent, so it never outlives a restart of the agent.
  `Workers::recover` therefore never takes a headless worker back: it kills what the tag still finds, as `pidfile::reap_orphans` does for jobs (`runner/pidfile.rs:119`), and reports the job `killed` with `retryable`, so its Run is `shed` and Spec 2's `launch_due` continues it with `--resume` and the same session id.
- With the headless host the workers live in the agent's cgroup, so `Agent::on_sample` passes the cgroup limit to the Linux room of Task 2 while `pick_host` gives the headless host for this device.
- `docs/WORKER-PROOF.md` gains the headless table: a start, the log through `idlemax logs --follow`, a commit, a refused `git push`, no MCP helper process, a stop, the same session continued, and an agent restart in the middle of a turn.

**Definition of Done:**

- [ ] `headless_args` returns exactly the two Claude lists above for a first start and a continued session, and `Unsupported` for Codex with a message that names the session id.
- [ ] With a stub CLI that reads its input to the end and prints it, `start` then `send` puts the sent line into `logs/jobs/<id>.log`, `state` reads `working` and then `done` with `exit_code` 0, and the log ends with the footer that makes `idlemax logs <id> --follow` return.
- [ ] A stub that prints `usage limit reached` and exits with code 1 gives `done` with `exit_code` 1, `end_text` holds that line, and the manager ends the job `failed` with the text in the last update.
- [ ] The stub's environment holds `IDLEMAX_JOB_ID`, `IDLEMAX_JOB_TAG` and `GIT_TERMINAL_PROMPT=0` and no `IDLEMAX_TOKEN`, and a stub started with `GH_TOKEN` set is never sent a brief.
- [ ] `stop` on a stub that ignores SIGTERM leaves no process of its group after the 5 s kill and leaves an uncommitted file in the worktree.
- [ ] A manager test over `HeadlessHost` and the stub ends a launch `succeeded`, and a stop followed by a resume passes the same session id with `--resume`.
- [ ] A manager built over a saved headless record reports a retryable kill for that job and leaves no process with its tag, and a record without `host` is read as a Herdr one.
- [ ] `pick_host` gives the headless host for `production` with every value of the key, for `auto` with no `herdr` on `PATH` and for `auto` with a `herdr` that does not answer, and the Herdr host for `auto` with one that answers; `Config::parse` refuses `workers.host = "herdr"` with `agent.profile = "production"` in a line that starts with `workers.host`.
- [ ] Verify: `cargo test -p idlemax-core worker::launch && cargo test -p idlemax-core config && cargo test -p idlemax --bin idlemax agent::worker && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 9: Give each device its own credential

**Objective:** Give each device's agent a credential of its own that opens only the `/v1/agent/*` routes and speaks only for that device (A15), and hold every server that still sends the owner token from another host.
A worker on a server then finds nothing on that server that opens an owner route.

**Files:**

- Create: `crates/idlemax-core/src/control/credential.rs`
- Modify: `crates/idlemax-core/src/control/mod.rs`
- Modify: `crates/idlemax-core/src/control/device.rs`
- Modify: `crates/idlemax-core/src/protocol.rs`
- Create: `crates/idlemax/src/coord/store/credentials.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/coord/store/devices.rs`
- Modify: `crates/idlemax/src/coord/api/http.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax/src/coord/api/plan.rs`
- Modify: `crates/idlemax/src/coord/api/devices.rs`
- Modify: `crates/idlemax/src/cli/device.rs`
- Modify: `crates/idlemax/src/cli/commands.rs`
- Modify: `crates/idlemax/src/cli/dispatch.rs`
- Modify: `crates/idlemax/src/cli/client.rs`
- Modify: `crates/idlemax/src/doctor/net.rs`
- Modify: `crates/idlemax/Cargo.toml`
- Modify: `crates/idlemax/tests/devices.rs`
- Modify: `README.md`, `docs/API.md`
- Test: `crates/idlemax-core/src/control/credential.rs` (inline tests), `crates/idlemax-core/src/control/device.rs` (inline tests), `crates/idlemax/src/coord/api/devices.rs` (inline tests), `crates/idlemax/tests/devices.rs`

**Key Decisions / Notes:**

- A device credential is a second kind of bearer token: 32 random bytes as hex from `generate_token` (`state.rs:165-174`), made by the coordinator and shown once.
  The agent side needs no new code: the credential goes into the device's `$IDLEMAX_HOME/token`, where `load_token` (`state.rs:113-122`) reads it, so `idlemax setup --role agent --token-stdin`, `idlemax init --token-stdin`, the agent's client (`agent/client.rs:110-114`) and the bearer check of the agent's local API work as they are.
  A server set up this way holds no owner token at all.
- The coordinator keeps only the SHA-256 digest of a credential, in the `device_credentials` table of Task 3's migration, so a copy of `coord.db` gives nobody a device's routes.
  `crates/idlemax/Cargo.toml` gains `sha2`, which is already in `Cargo.lock`, so nothing new is downloaded; the digest is computed in the adapter (`coord/api/http.rs`), and the core stays free of it.
  `coord/store/credentials.rs` adds `Store::set_device_credential(device, digest, now)`, which replaces an older one, `Store::delete_device_credential(device)` and `Store::credential_device(digest)`.
- `control/credential.rs` is the pure part:

  ```rust
  pub enum Identity { Owner, Device(DeviceId) }
  #[serde(rename_all = "snake_case")]
  pub enum AgentAuth { Credential, OwnerTokenLocal, OwnerTokenRemote }
  /// A device credential opens `/v1/agent/*` and nothing else.
  pub fn route_allowed(identity: &Identity, path: &str) -> bool;
  /// A device credential speaks for its own device only.
  pub fn speaks_for(identity: &Identity, claimed: &str) -> Result<(), String>;
  pub fn agent_auth(identity: &Identity, same_host: bool) -> AgentAuth;
  ```

- `require_bearer` (`coord/api/http.rs:157-171`) stays for the agent's local API; the coordinator's router (`coord/api.rs:80-106`) uses the new `require_access` instead.
  It compares the bearer with the owner token first, in memory and in constant time as today, and gives `Identity::Owner`; else it looks the bearer's digest up in the store and gives `Identity::Device(id)`; else it answers 401.
  A device identity on a path `route_allowed` refuses gets 403 through `ApiFailure::Forbidden`, with the text `this credential belongs to device <id> and opens only the agent routes; run owner commands from a device that holds the owner token`.
  The identity travels to the handlers as a request extension.
- The four agent handlers check `speaks_for` against the device their body names and answer 403 on a mismatch: `heartbeat`, `job_update` and `advisor_inputs` (`coord/api/agents.rs:25-78`) and the dispatch-inputs handler of Spec 1 (`coord/api/plan.rs`).
  The owner token keeps opening the agent routes, so one machine with a coordinator and an agent, and every fleet that exists today, keeps working for plain jobs with no change.
- The heartbeat handler stores how the agent proved itself: `agent_auth` of the identity and Spec 4's `same_host` for the peer address, written into `devices.agent_auth` by `record_heartbeat` and served as `DeviceView.agent_auth: Option<AgentAuth>` (`protocol.rs:162-182`).
- `device_gate` (`control/device.rs`, Task 3) gains its second rule, after the switch: a device whose profile is not `laptop` and whose `agent_auth` is `OwnerTokenRemote`, or not known yet, is held with `server <id> has no credential of its own: idlemax device credential <id>`.
  So the Plan, the feed reply, the pass, every dispatcher job and every launch treat such a server like an off device (A15: no server runs before that credential exists).
  A `laptop` keeps the owner token for its agent: the owner's commands need that token on the laptop in any case, so a second credential there would protect nothing (Open Question 1).
  An agent on the coordinator's own host may keep the owner token too, because there Spec 4's caller check still sees the calling process.
- The routes: `POST /v1/devices/{id}/credential` makes a new credential for an id that passes `name_problem`, known or not, since a new server has never sent a heartbeat, stores its digest and answers `{"device", "credential"}` once; `DELETE /v1/devices/{id}/credential` removes it and answers `{"device", "removed"}`.
  Both call `CallerCheck::ensure(peer, Access::Owner)` and append one `EventKind::Control` event that names the device and never the credential.
- `idlemax device credential <id>` prints the credential with the next step, `--raw` prints the credential alone for a pipe into `idlemax setup --token-stdin` on the server, and `--revoke` removes it; `access_of` classes all three as `Owner`.
  The credential is never passed in an argument list.
  This is an addition under the `device` command this spec owns, next to `on` and `off` of the shared command list.
- `idlemax doctor` on a device that holds a credential: the coordinator check (`doctor/net.rs:13`) reads the 403 text above as `ok` with `this device holds its own credential; owner commands run from a device with the owner token`, in place of the failure its 401 and 403 arm reports today (`doctor/net.rs:115`).
- Docs in this task: `docs/API.md` "Auth and errors" gets the two kinds of bearer, the 403 text, the two routes and `DeviceView.agent_auth`; `README.md` "Safety model" says what a device credential opens and that a server keeps no owner token.

**Definition of Done:**

- [x] `route_allowed` passes every path for `Owner`, passes `/v1/agent/heartbeat` for a device and refuses `/v1/status`, `/v1/devices/x/on` and `/` for it.
- [x] A request with a device credential gets 200 on `POST /v1/agent/heartbeat` for its own device, 403 on the same route with another device id in the body, 403 on `GET /v1/status` and 403 on `POST /v1/devices/<id>/on`; a request with an unknown bearer gets 401.
- [x] After `POST /v1/devices/<id>/credential` the store holds a digest and not the credential, a second call makes the first credential answer 401, and `DELETE` makes the second one answer 401.
- [x] A heartbeat with the owner token from a loopback peer stores `owner_token_local`, one from another address stores `owner_token_remote`, and one with the device's credential stores `credential`.
- [x] `device_gate` holds a `workbox` that is on with `owner_token_remote` and with no `agent_auth`, and gives `None` for the same device with `credential`, for a `workbox` with `owner_token_local` and for a `laptop` with `owner_token_remote`.
- [x] In `tests/devices.rs`, an agent started in a second temp home whose `token` file holds the output of `idlemax device credential <id> --raw` appears online, runs one plain job, and `idlemax status` from that home exits non-zero with the 403 text.
- [x] `idlemax doctor --json` in that second home reports the coordinator check as `ok` with the credential text.
- [x] `docs/API.md` and `README.md` describe the credential.
- [x] Verify: `cargo test -p idlemax-core control::credential && cargo test -p idlemax-core control::device && cargo test -p idlemax --bin idlemax coord::api::devices && cargo test -p idlemax --bin idlemax doctor && cargo test -p idlemax --test devices && cargo test -p idlemax --test e2e && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 10: Refuse a public coordinator address and write the server rollout

**Objective:** Make "reachable only over the owner's private network" a rule the coordinator enforces at start, and write down how the coordinator moves to an always-on server and how a work server joins.

**Files:**

- Modify: `crates/idlemax-core/src/host.rs`
- Modify: `crates/idlemax/src/coord/mod.rs`
- Modify: `crates/idlemax-core/src/install/setup.rs`
- Modify: `docs/ROLLOUT.md`
- Modify: `README.md`, `docs/API.md`
- Test: `crates/idlemax-core/src/host.rs` (inline tests), `crates/idlemax/src/coord/mod.rs` (inline tests), `crates/idlemax/tests/setup.rs`

**Key Decisions / Notes:**

- `listen_scope(ip)` in `host.rs` is pure and returns `Loopback`, `Private` or `Public`.
  Private is the private IPv4 ranges of RFC 1918, the shared range of RFC 6598 that tailnets use, link-local addresses and the IPv6 unique local range; a mapped IPv6 address is looked through first, as the wildcard check does (`coord/mod.rs:71-72`).
- `listen_addr` (`coord/mod.rs:67-76`) refuses a `Public` address with the new `CoordError::PublicListen`, whose message says to bind the server's private-network address, the one section 3.1 of `docs/ROLLOUT.md` finds (D19).
  There is no key that turns the check off.
  `idlemax setup --role coord` fails the same way while it plans (`install/setup.rs:224-246`), so a wrong address is caught before a service is written.
- `docs/ROLLOUT.md` gains a section "The dispatcher on servers" that builds on its sections 0 to 5 (roadmap items 1 and 2) and says the following, in this order.
  The coordinator runs on one always-on server, and every other device, the Mac included, is one of its agents.
  The coordinator is upgraded before any agent, because of the profile names of Task 1.
  The settings the coordinator reads move with it: `[[quota.accounts]]`, the other `[quota]` keys, `[workers]`, `[dispatcher]`, `[advisor]` and the file `rules.md` are copied from the old coordinator's state dir to the new one, because a fresh server config has no account and an empty `workers.clis`.
  The coordinator runs in the owner's time zone, so Spec 4's daily summary goes out at 09:00 for the owner (A22): on a host that runs other services a drop-in sets `Environment=TZ=<zone>` for the coordinator unit alone, and on a work server the owner may set the system zone with `timedatectl set-timezone` instead.
  A work server runs its agent as the owner's login user, because workers need that user's repositories, agent CLI logins and Herdr session.
  It is set up with `idlemax device credential <id> --raw` on a device that holds the owner token, piped into `idlemax setup --role agent --profile workbox --coord-url <url> --token-stdin` on the server, and kept running from boot with the `loginctl enable-linger` step that `idlemax service install` already names (`cli/service.rs:355-357`).
  A server whose Herdr session is not running after a reboot runs its workers headless until the owner starts the session again.
  The packaged root unit with its own system user and `ProtectHome=read-only` (`packaging/linux/idlemax-agent.service:19`, `:48`) stays the `production` route; such a device has no agent CLI login and cannot write the owner's repositories, so it starts no worker, and its login check says why.
  Each agent CLI is logged in on each server as the login user, and `idlemax doctor` shows the `login.*` checks.
  A server is put to work with `idlemax device on <id>` and `idlemax project on <name> --device <id>`, and taken out with `idlemax device off <id>`.
  The mode stays `off` or `propose` after the move until the owner has answered the question of the worker fence (A13, A15).
- Docs in this task: `docs/API.md` "Coordinator" says a public listen address is refused, and the README "Deploying across devices" points at the new rollout section.

**Definition of Done:**

- [x] `listen_scope` returns `Loopback` for both loopback forms, `Private` for one address of each private range, the shared range, link-local and unique local, and `Public` for a documentation-range IPv4 address (RFC 5737) and a documentation-range IPv6 address (RFC 3849).
- [x] `listen_addr` refuses a public address with a message that names the private-network address and `docs/ROLLOUT.md`, and still accepts every address its existing test accepts (`coord/mod.rs:98-126`).
- [x] `idlemax setup --role coord --coord-url` with a public address fails in `--dry-run` with the same message and writes nothing.
- [x] `docs/ROLLOUT.md` has the section with each step above, and no address, host name or device id of a real machine.
- [x] Verify: `cargo test -p idlemax-core host && cargo test -p idlemax --bin idlemax coord::tests && cargo test -p idlemax --test setup && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`

### Task 11: Needs the owner's OK: move the coordinator to the always-on server

**Objective:** Carry out roadmap items 1 and 2 on the real server and make the Mac one of the coordinator's agents.
This changes real services on the server and on the Mac, so it runs only with the owner's OK and the owner runs each privileged step.

**Files:**

- Modify: `docs/ROADMAP.md`
- Modify: `docs/ROLLOUT.md`

**Key Decisions / Notes:**

- Which machine hosts the coordinator is the owner's choice between two roles, and the steps differ (Open Question 6).
  Role A is the server of roadmap items 1 and 2 and of `docs/ROLLOUT.md`, which runs sites that must keep running (`docs/ROLLOUT.md:4`): it is a `production` device, and section 3.2 installs the coordinator and the agent there as root units of the system user `idlemax`.
  Role B is one of the two work servers of D20: then this task installs only the coordinator unit there, and Task 12 installs that host's one agent as the login user with the `workbox` profile, so two agents never report as one device (D19: one agent per device).
- The steps are those of `docs/ROLLOUT.md`: the prerequisites of section 0, a release binary or the capped build of section 1, the verification script of section 2 as the login user (it uses its own temp home and never touches real state), the backups of section 5 before the first start, then section 3 with the coordinator bound to the server's private-network address (D19).
- The coordinator build comes first and the Mac's agent second, because of the profile names of Task 1.
- One coordinator for all (D19): the Mac's coordinator service is stopped, its `coord.db` is copied to the server with the `.backup` command of section 5 while both coordinators are stopped, so Runs, account days, projects and switches survive, and the Mac's coordinator unit is removed with `idlemax service uninstall coord`.
- The settings move too, as the rollout section of Task 10 lists them: the `[[quota.accounts]]` entries and the other `[quota]` keys, `[workers]`, `[dispatcher]`, `[advisor]` and `rules.md` with its approved lines are copied into the server's state dir before its first start.
  Without them the moved coordinator shows no Budget and launches nothing.
- The coordinator unit gets the owner's time zone as the rollout section says (A22).
- The Mac keeps the owner token: it gets the server's token with `idlemax init --token-stdin` and its `coordinator.url` as section 3.3 says; restarting the Mac's LaunchAgent is the owner's own action.
  The Mac is switched on again with `idlemax device on <id>` when the owner wants it in the Plan, since every device is off after the migration (A23).
- The dispatcher mode is set to `propose` or `off` before the Mac's coordinator stops, and stays there.
  From this task on a call from the Mac is judged by the token alone, so the mode goes back to `auto` only by the owner's own `idlemax mode auto`, after the owner has answered A13 (A15, Open Question 1); the date of that answer goes under roadmap item 37.
- Section 3.4 and the 24-hour check of section 4 are run as written, with the Mac kept awake for that day.

**Definition of Done:**

- [ ] Every box of section 7 of `docs/ROLLOUT.md` for roadmap items 1 and 2 is ticked with the date.
- [ ] `idlemax status --json` on the Mac shows the Mac online against the server's coordinator and exactly one device for the coordinator's host, and `idlemax doctor` is healthy on both machines.
- [ ] `idlemax quota` on the Mac lists the same accounts and budgets as before the move, and `idlemax project list` the same projects.
- [ ] `idlemax plan --json` against the moved coordinator shows the mode `off` or `propose`, and `idlemax jobs --json` holds no worker job.
- [ ] `idlemax service status` on the Mac shows no coordinator service, and a request from outside the private network to the coordinator's port fails.
- [ ] The first daily summary after the move carries the owner's local date and arrives at 09:00 of the owner's clock.
- [ ] Roadmap items 1 and 2 in `docs/ROADMAP.md` record the result and which role the coordinator's host has, without its name.
- [ ] Verify: `idlemax status --json && idlemax doctor && idlemax quota --json && idlemax plan --json`, run by the owner; `git status --short` lists only `docs/ROADMAP.md` and `docs/ROLLOUT.md`, and the task adds no code and no test, so `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass as before.

### Task 12: Needs the owner's OK: join both servers as workbox and prove the dispatcher there

**Objective:** Install the agent on the owner's two Linux servers with the `workbox` profile and each server's own credential, log their agent CLIs in, switch the devices and their projects on, and prove on one of them that the readings, the lines, the headless runner, the shared Budget and the zero-quota checks behave as built.
This installs services, uses real logins, starts real agent sessions and spends a small amount of real quota, so it runs only with the owner's OK.

**Files:**

- Modify: `docs/WORKER-PROOF.md`
- Modify: `docs/ROADMAP.md`

**Key Decisions / Notes:**

- Both servers are work machines with no live services for customers, so the owner sets both to `workbox` at setup with `--profile workbox`; nothing in the code knows their names (D20).
- Part 1, join, per server, as the owner's login user and by the rollout section of Task 10: install the release binary, pipe `idlemax device credential <id> --raw` from the Mac into `idlemax setup --role agent --profile workbox --coord-url <url> --token-stdin --allow <programs>` on the server, run the `loginctl enable-linger` step, and log in each agent CLI the server should use.
  The server's `token` file then holds its credential and no owner token.
  A server whose workers run in Herdr panes also gets `idlemax statusline install`, after its `--dry-run` was read, because the Claude quota reading comes from that capture (Open Question 3).
  `idlemax doctor` on the server must show the `login.*` checks as `ok` before the device is switched on, and `idlemax quota` on the Mac must show no closing account problem.
  Then the owner runs `idlemax device on <id>` for each server and `idlemax project on <name> --device <id>` for each project it should work on, with the dispatcher mode still `propose`, and reads `idlemax plan`.
  The login user of a work server is the user Spec 2's open question on the worker's OS user is about; this part runs under the soft fences, and the owner's OK for it is recorded with its date in `docs/WORKER-PROOF.md` (A13).
- Part 2, the headless proof, runs on one server in a throwaway `IDLEMAX_HOME` on spare ports, asserted before every command as `AGENTS.md` shows, with a temp git repository and a local bare remote.
  The real `HOME` stays because the agent CLI needs its login, so `idlemax statusline install`, `idlemax hooks install`, `idlemax service install` and every `systemctl` call must not run in this part: they would edit the real settings of that user.
  It fills the headless table of `docs/WORKER-PROOF.md` for Claude: the log through `idlemax logs --follow`, a commit, a refused `git push` with the bare remote unchanged, no MCP helper process, a stop freeing every process within 10 s, the same session id continued, which proves `--session-id` with `--resume` after a kill (D11, D19), and an agent restart in the middle of a turn.
  It records what a logged-out `claude auth status --json` and `codex login status` print on Linux against Task 5, how `codex exec --json` shows the session id of a new run, and whether a `herdr` call starts the Herdr server by itself; with that, Codex gets its row in `headless_args` or stays `Unsupported`.
  The temp directory and every daemon started for this part are removed at its end.
- Part 3, the real install in `propose`: the cgroup limit and use and the pressure figures in `idlemax local status --json` are compared with `memory.max`, `memory.current`, `memory.stat`, `/proc/pressure/memory` and `/proc/meminfo` on that server.
  The Mac and a server on one login show one Budget in `idlemax quota` (D19).
  A Claude session the owner opens in a Herdr pane on the server refreshes that account's reading, which is the assumption Truth 1 rests on.
- Part 4 needs `auto` and runs only after the owner has answered A13 and switched the mode with `idlemax mode auto` (A15).
  Workers are added until the line state reads `hold`, and the available memory, both pressure figures and the unit's `pids.peak` are recorded at that moment.
  The `workbox` numbers of Task 2 and `WORKBOX_TASKS_MAX` of Task 1 are changed where the record shows the reserve was crossed, room was left idle or the peak was above half the limit, and the changed values go into the README table of the lines.
  A Run on the server lowers the `left_pct` the Mac shows.
  With the laptop asleep, the server finishes one task with a verdict, which is the "done when" of roadmap item 37.
  On a day whose budget reads `spent`, a check job runs on the server and `idlemax plan` then shows a `broken_check` task or none because every check passed.

**Definition of Done:**

- [ ] `idlemax status --json` lists both servers online with the profile `workbox`, `enabled` true and `agent_auth` of `credential`, and no server's `token` file holds the owner token.
- [ ] `idlemax doctor` on each server is healthy and shows each used agent CLI as logged in.
- [ ] `idlemax plan` gives each server its line state and, for each switched-on project, a pick or the reason the slot is empty.
- [ ] `docs/WORKER-PROOF.md` holds the date of the owner's OK, a dated result for every row of the headless Claude table and the readings comparison.
- [ ] It records one Budget shown for two devices on one login, and that a pane session on the server refreshed the reading.
- [ ] After the answer to A13: it records the available memory, the pressure figures and `pids.peak` at the hold line with the numbers idlemax uses after tuning, a Run on the server lowering the Budget the Mac shows, a task finished on the server while the laptop slept, and one check job run on a spent day.
- [ ] Until that answer, roadmap item 37 in `docs/ROADMAP.md` records parts 1 to 3, says that part 4 waits for it, and stays open.
- [ ] No server's device id appears in the repository: `git grep -n <id>` returns nothing for each.
- [ ] Verify: `idlemax status --json && idlemax plan --json` on the Mac, run by the owner, and `grep -n "Result:" docs/WORKER-PROOF.md`; `git status --short` lists only `docs/WORKER-PROOF.md` and `docs/ROADMAP.md`, and the task adds no code and no test, so `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass as before.

## Open Questions

1. **How strong is the worker fence, and when may the moved coordinator run in `auto` (A13, pending).**
   A worker runs as the OS user of its agent, so it can read any token file that user can read.
   This plan removes that for servers: a server holds only its own credential, which opens the agent routes alone (Task 9).
   It cannot remove it for the Mac, where the owner's own commands need the owner token on disk, and once the coordinator runs on another machine Spec 4's caller check judges a call from the Mac by the token alone (`classify`: a caller that is not on this host is `Owner`).
   On a work server the agent also runs as the owner's login user, with that user's keys, `gh` login and repositories in reach, which is the user Spec 2's first open question is about.
   Recommended: the owner answers A13 before Task 11 runs; if the coordinator moves first, the mode stays `off` or `propose` on it, for the Mac too, until the answer, and a dedicated OS user or a sandbox for workers is then built by Spec 2 for the Mac and the servers alike.
   If the owner would rather keep the Mac in `auto` while the servers wait, the smallest change is one more rule in `device_gate` that holds every device that is not a `laptop` until a setting says otherwise, and this file changes first.
2. **`orgId` is the only id in the Claude login reply besides the email address.**
   The reply holds no per-user id (the key list is in Task 5), so two seats of one team organization would read as one login.
   Whether one `orgId` always means one personal subscription is not verified, and the mistake is on the safe side: one shared Budget, or `OneLoginTwoNames` closing the second name.
   Recommended: use `orgId`; if the owner ever holds two logins in one organization, add a one-way digest of the email address as a second part, never the address itself.
3. **A print-mode Claude run does not refresh the quota reading.**
   The code notes that `claude -p` runs never refresh the statusline capture (`crates/idlemax-core/src/quota/rules.rs:15-17`), and Spec 3 closes an account whose reading is older than 6 hours with the basis `no_data`.
   So an account that only headless workers use closes after 6 hours, and Truth 1 holds only while some pane session on a server refreshes the capture, which is unproven until Task 12.
   Recommended: accept the fail-closed result, keep Herdr panes as the normal host on both work servers with the capture installed, and add a reader for print-mode runs as a follow-up once Spec 3's measurements say what such a run reports.
4. **Headless Codex has no known session id.**
   `codex exec` has `resume [SESSION_ID]`, `--json`, `--model` and `--sandbox` (`codex exec --help`, 2026-11-12), but nothing names the session id of a new run without reading its event stream, and that stream was not observed.
   Recommended: Claude only in Task 8, and a Codex row once Task 12 has recorded the event that carries the id.
5. **Does the 5-hour stop apply to a server-only account.**
   D19 says a server-only account "works all week", and A18 stops every worker of an account at 60 percent of its 5-hour window, a line that exists so the owner is not locked out (D10 item 4).
   On an account the owner does not use, the stop protects nobody and leaves up to 40 percent of every 5-hour window unused.
   Recommended: the stop line of a server-only account becomes 100 minus `quota.server_only_margin_pct`; that needs a second override on Spec 3's `BudgetInput`, so it is not built here until `docs/DISPATCHER.md` says so, and until then Task 6 keeps the stop at 60.
6. **Which machine hosts the coordinator.**
   `docs/ROLLOUT.md` and roadmap items 1 and 2 are written for a server whose sites must keep running, D20 says the two saved servers run no live services, and the repository cannot say whether these are two machines or three.
   Recommended: role A of Task 11, the server of roadmap items 1 and 2, because its units, caps and 24-hour check are already written and proven for it; Task 11 covers role B with the one-agent rule if the owner picks a work server instead.
7. **May one device finish another device's lapsed job.**
   `is_late_result` (`scheduler/update.rs`) applies a result for a requeued job (no device, `attempt` equal to `job.attempts`) from any authenticated agent, so a device credential can end another device's lapsed job if it knows the id and the attempt, and a `succeeded` orchestrator pass with `output_tail` would be ingested.
   Recommended: record the device of the last lease on requeue and refuse a late result from any other device; not built here because it changes Spec 1's lease rules.

## Review Disposition

1. fixed: Task 7 keeps only the pure "when" (`budget_spent`, `checks_due`) and resets the project's `base_checks` row, so Spec 1's `next_step`, `check_job`, `step` and `discover` do the rest (A11); `WorkerAction::Check`, the worktree phases, the `JobSpec::validate` exception, the hook in `coord/api/agents.rs` and `Store::newest_job_by_key` are gone.
2. fixed: Task 3's `device_gate` is asked by `build` (as `DevicePlan.held`), `pass_due`, the dispatch-inputs reply, `step` and `lease_lines`, switching off ends every dispatcher job of the device (A11), no orchestrator pass is placed on an off device, and the Definition of Done has the `step` box.
3. fixed: the credential is Task 9, before the move; Task 11's Definition of Done holds the mode at `off` or `propose`, the Context sentence and the Risk row name the Mac, and what the credential cannot close for the Mac is Open Question 1 under A13.
4. fixed: `cli/service/target.rs`, `assets/dashboard.js` and `README.md:194` are in Task 1 with the decisions, and Verify runs the test files the rename reaches.
5. fixed: every description of Spec 1's code is replaced by its path and symbol; the Linux result is passed beside the snapshot, a failed reading gives its own reason, and the swap-in rule is off on Linux in favour of pressure (Task 2).
6. fixed: `linux_room` returns the budget and the slot count uses it, and own use is the cgroup's use plus the tracked use outside it, each with a Definition of Done line (Task 2).
7. fixed: below the reserve the Linux memory line reads `hold` while nothing of idlemax's is tracked, with the 3.3 GiB case as a Definition of Done line (Task 2).
8. fixed: `server_only` is false when a `laptop` is on the same Claude login or may be, with the `OwnerMayUse` warning for the unknown case, and the task says Codex has no such check (Task 6).
9. fixed: `DeviceInfo.claude_login` is the three-state `Login`, the agent keeps the last good reading for a day, and `account_problems` has `LoginUnknown` (Tasks 5 and 6).
10. fixed: the login command runs only when the device's own config holds a `claude` account, the Definition of Done pins that a stub is not run without one, and `tests/doctor.rs` is named with `cargo test -p idlemax --test doctor` in Verify (Task 5).
11. fixed: Spec 1 now builds `--device` on every project command and the `?device=` filter, so Task 4 builds no project code and says so.
12. fixed: the `on` handler calls `CallerCheck::ensure(peer, Access::Owner)`, `access_of` classes both commands, Task 3 has the 403 box, and Task 4 wires the dashboard and TUI toggle to the two routes with a box for each.
13. fixed: the rollout section and Task 11 copy the settings and `rules.md`, with the `idlemax quota` box.
14. fixed differently: Task 11 names both possible roles of the host with the one-agent rule and the `idlemax status` box, and which role it is stays Open Question 6, because the repository cannot say.
15. fixed: `workbox()` takes the worker-line pressure values for its guard (Task 1).
16. fixed: `auto` falls back to the headless host when Herdr does not answer, the rollout section covers a reboot, and `record.rs` is in Task 8 with the restart box.
17. fixed: `sized_unit` writes a `TasksMax` sized by profile, with a test (Task 1).
18. fixed: the new tests sit in `coord/api/devices.rs`, `doctor/logins.rs`, `control/device.rs` and `control/credential.rs`, and Verify uses their narrow filters.
19. fixed: the Linux step is `crates/idlemax-core/src/dispatch/linux.rs`.
20. fixed: the 404 is called a new status and every client percent-encodes the id (Tasks 3 and 4).
21. fixed: `Config::validate` refuses `workers.host = "herdr"` beside `agent.profile = "production"` (Task 8).
22. fixed: part 2 of Task 12 names the three install commands and `systemctl` as commands that must not run there.
23. fixed: the pane-session dependency is under Assumptions with Task 12 as its proof, and the 5-hour stop of a server-only account is Open Question 5.
