# idlemax roadmap

v0.1.0 shipped the coordinator, the agent, the CLI, the TUI and the web dashboard, built and tested on macOS.
v0.2.0 shipped everything in the status section below, with prebuilt binaries for macOS and Linux.
This roadmap collects every gap found while building and reviewing it, every `SHORTCUT:` marker in `crates/`, and the open items from the design research.
Each open item says why it matters, how to do it, and when it counts as done.
Item numbers are stable, so commit messages and notes can refer to them.
Re-harvest the shortcut ledger with `grep -rnE '(#|//) ?SHORTCUT:' crates` before planning a release.

## Status after the second round (2026-11-08)

CI runs fmt, clippy and the full test suite on macOS and Linux for every push to `main` that changes more than docs.
The Linux agent was verified on an Ubuntu 24.04 host with systemd 255 (see item 1).

### Roadmap items landed

| Item | What changed | Commit |
|---|---|---|
| 3 | Each agent sends the GitHub summary and session metadata to the coordinator | `da05dea` |
| 4 | Statusline wrapper proof, byte-for-byte passthrough, `idlemax statusline install` | `54e2cdd`, `e780430`, `0fa6d7f` |
| 5 | Job updates are journaled before sending and applied once after a crash | `e06a009` |
| 6 | Dashboard checked in a browser, with layout, refresh and contrast fixes | `02f8744` |
| 8 | Control and advice-accept writes are atomic | `b021525` |
| 9 | Job runtime counts awake time only (logic and tests; a real sleep is item 9 below) | `22cda05` |
| 10 | Hourly device refresh, separate local-pause reason, and the event loop split | `f034e80`, `323427e` |
| 12 | Interactive agy sessions hold their account | `d925e93` |
| 13 | The AI pass prompt travels on stdin, with user hooks and MCP servers off | `9ebc796` |
| 14 | Daemonizing job children count against the cap, matched by a random per-attempt tag | `8ba59e3`, `c9a61ec` |
| 15 | Device presence survives a restart, and the server has header-read and request timeouts | `7ed16d9` |
| 18 | A job's real exit is reported after the agent died, and the other outbox gaps are closed | `1dbd25a` |

### New since v0.1.0, beyond the roadmap

| Area | What changed | First commit |
|---|---|---|
| CI and releases | fmt, clippy and tests on macOS and Linux, a tag-triggered release, `scripts/install.sh` | `c33000e`, `8a45997` |
| Setup | `idlemax setup`, `service install`, `statusline install`, `doctor` | `df562c7`, `0fa6d7f`, `26eee5c` |
| Following work | `idlemax why` in the CLI, TUI and dashboard, `logs`, `inbox`, recurring `schedule` | `fb0abbb`, `cd3ebcc`, `97e06d6`, `04ce632`, `f1aa4eb` |
| Agents | TOON output, structured errors, no-ops, home view, session hooks, generated skill, `job add --wait`, `AGENTS.md` | `e310996`, `c3d937a`, `1e8c2ec`, `916c772`, `d98e321`, `e19c504` |
| Job policy | A failure is final; only causes that are not the job's fault are retried | `cb87737` |
| Linux | Jobs no longer hold on unknown idle time, the systemd user manager is found by either socket, a job over its cap is killed by the cgroup | `1786f23`, `9cc51f7`, `3ff5eae` |
| Security | An audit with fixes: accept errors no longer stop the server, stdin can no longer freeze the agent, private file modes, escape stripping | `d2e3221`, `47dfcbd` |
| TUI | Every screen fits 80x24, display-width text, time-zone independent snapshots, `--utc` | `3057446`, `b170592` |
| Community | CONTRIBUTING, SECURITY, issue forms, Dependabot, private vulnerability reporting | `ed051c9` |

### Round 3 (2026-11-09): maximum use, properly

Four read-only audits and a worker fleet landed about 50 fixes; `git log v0.2.0..` lists them. The main ones:

- Guard: disk only holds new starts at 2 GiB and kills near 1 GiB; swap in use no longer stops Linux servers; load limits scale with CPU count and ignore the jobs' own load; the laptop takes half its CPUs and a quarter of its RAM.
- Agent: a stuck mount no longer freezes the loop; a finished slot is refilled at once; sleep, restart, a second agent, an early exit or a clock step no longer lose, re-run or mislabel a job; outbox sequence numbers survive.
- Coordinator: leases use `lease_secs` and are renewed by heartbeats only; unstarted leases return to the queue; unknown pause targets are refused; schedule, quota and pause-flag edge cases fixed.
- CLI, setup and doctor: strict spec files, safe `init`, right recovery hints, `--fields` checked everywhere, Linux service restart, idle-agent and PATH checks in doctor.
- CI skips docs-only pushes and cancels superseded runs.

### Needs the owner's OK before it runs

- Running the Linux verification on the production server (item 1) and deploying on the tailnet (item 2).
- Installing the statusline wrapper into the real Claude settings (item 4).
- One supervised AI pass, which spends quota (item 7).
- A real sleep test on the Mac (item 9).

## v0.3: run it for real on the Mac and the server

### 1. Verify the Linux agent on the production server

- **Why:** The Linux path is proven on a development host, not yet on the production server that also serves sites that must never be starved.
- **Done so far:** On an Ubuntu 24.04 host with systemd 255, `scripts/verify-linux.sh` passed every check: kernel signal comparison, the capped user scopes, a memory hog killed at `MemoryMax` with the cap named in its error, a CPU job held to its quota, and `systemd-analyze verify` on both units.
  The end-to-end test and the full suite pass there and in CI on Linux.
- **How:** Check the server's systemd version first: the verification script needs 254 or newer, `OOMPolicy=kill` is added only on 253 or newer, and `--expand-environment=no` only on 254 or newer.
  Then run `scripts/verify-linux.sh` on the server as the normal deployment user, following `docs/ROLLOUT.md`, with a binary built inside a capped transient scope or taken from a release.
- **Done when:** The same checks pass on the production server, and its sites show no slowdown during the run.

### 2. Deploy the coordinator on the tailnet

- **Why:** The coordinator has only run on loopback, and production binds the server's tailnet address under the capped unit.
- **How:** Follow `docs/ROLLOUT.md`: `idlemax setup --role coord` on the server with `coordinator.listen` set to the tailnet address, the shared token to each device with `--token-stdin`, and `idlemax setup --role agent --coord-url <server>` on the Mac.
  Once, before the 24-hour run, point a v0.1.0 agent at the new coordinator, because old-agent compatibility rests on serde defaults and API tests only.
- **Done when:** The Mac agent heartbeats to the server over the tailnet for 24 hours without a gap.
  The API and dashboard are unreachable from outside the tailnet.
  The coordinator stays under `MemoryMax=256M` and `CPUQuota=20%` for that whole day.

### 4. Install and verify the Claude statusline capture

- **Why:** The wrapper and its install command are done and tested, but nothing is installed yet, so Claude accounts stay unspendable (fail closed).
- **How:** `idlemax statusline install --dry-run`, check the diff, then `idlemax statusline install`.
- **Done when:** A Claude account shows a fresh snapshot in `idlemax quota` within a minute of interactive use, and the visible statusline output is unchanged.

### 7. Run one supervised AI pass end to end

- **Why:** The `claude -p` advisor pass and the ingest of its output have never run, because a run spends quota.
  The pass sends its prompt on stdin and runs with `--settings {"disableAllHooks":true} --strict-mcp-config`, which has only been checked against the documented flags and tests.
- **How:** Allowlist `claude`, list the AI account in `agent.ai_accounts`, and wait until the quota is spendable.
  Then let the advisor queue its pass, watch it run under the guard, and review what it ingests.
- **Done when:** One AI pass produces validated recommendations in `idlemax advise` without a retry, its quota use shows in `idlemax quota`, and no user hook or MCP server started during the pass.

### 9. Prove the awake-only runtime on a real sleep

- **Why:** Runtime uses the monotonic clock, which does not advance while the machine sleeps on macOS and Linux (confirmed in the Rust standard library source), and an injected-clock test covers the logic, but a real sleep has not been tried.
- **How:** Start a job with a short `max_runtime_secs`, let the Mac sleep for longer than that, wake it, and watch the job.
- **Done when:** The job is still running after wake and is killed only once its awake runtime passes the limit.

### 19. Finish the dashboard checks

- **Why:** The browser passes used Chrome only.
- **How:** Open the dashboard in Safari and Firefox and on a real phone, including a job with many waiting reasons and a status filter that matches nothing.
- **Done when:** No layout or control defect shows in any of them.

### 21. Show how much capacity is used and how much sits idle

- **Why:** The main goal is maximum use of every resource; round 3 made the guard and scheduler use far more of each machine, but a user still cannot see at a glance how many slots and how much RAM budget each device has free, or why a device with queued jobs is idle.
- **How:** From data the API already returns (guard slots and `mem_mib`, running jobs, `why`), show per device "slots in use / allowed" and the top idle reason in `idlemax status`, `devices`, the TUI and the dashboard, and per AI account the quota left before reset.
- **Done when:** On a busy cluster, each view answers "how much is used, how much is idle, and why" without running `why` per job.

### 22. Never block the agent loop on a hung mount at job start (done)

- **Why:** The disk probe is now bounded, but `resolve_cwd` in `agent/runner/launch.rs` still calls `is_dir()` on the job's cwd inside the loop, so a job whose cwd sits on a hung network mount stalls the loop at start.
- **How:** Check the cwd on a bounded blocking thread (as the sample does) and fail the start with a clear error when it times out.
- **Done when:** A test with a cwd check that never returns starts no job, reports the cause, and keeps the loop responsive.

### 23. Let the server preset scale with the host

- **Why:** The laptop preset now takes half the CPUs and a quarter of the RAM, but `linux_server` stays at 1 job and 512 MiB to fit the packaged agent unit's `MemoryMax=640M`, so a big server idles most of its spare capacity.
- **How:** Size the server preset from the host like the laptop one, with the site-protection reserve explicit, and derive the unit's `MemoryMax` from the same budget at install time.
- **Done when:** A 16 GiB server runs several jobs by default while its own services keep their reserve, and the unit cap matches the budget.

### 24. Bound every coordinator write path

- **Why (audit, round 3):** a token holder can still grow `coord.db`: heartbeat `quota`, `running` and `interactive_use` lists are unbounded; snapshots under never-configured account names are never pruned; `JobUpdate.device` is stored unchecked; advisor-input device ids are uncapped; account-scope pause names are not syntax-checked; schedules have no count cap.
- **How:** `name_problem` on those names, bounded lists, drop snapshots for accounts not in `[[quota.accounts]]`, count advisor-input devices against the device cap, a schedules cap.
- **Done when:** Each path has a test that an oversized or over-count request is refused or trimmed.

### 25. Serialize the advisor refresh (done)

- **Why (audit, round 3):** `run_refresh` reads and persists in two lock acquisitions, so a sweep refresh and `POST /v1/advice/refresh` can interleave: two AI passes queued, duplicate advice, an advice accepted mid-refresh set back to dismissed, or an auto-accepted advice whose job never queued.
- **How:** One `tokio::Mutex` around refreshes in `AppState`, close only `WHERE status = 'open'`, persist in one transaction, give the AI pass an idempotency key. Test without a real AI pass.
- **Done when:** Two concurrent refreshes in a test queue at most one pass and never change an accepted advice.

### 26. Explain jobs outside the store lock

- **Why (audit, round 3, unconfirmed):** `GET /v1/why` runs `explain` (which calls `scheduler::pick`) for up to 1000 jobs inside the store lock, so a dashboard poll on a big queue could delay heartbeats past the agent's 10 s timeout.
- **How:** Time it first with 10,000 queued jobs and 2 devices; if slow, snapshot under the lock, explain outside it, and pick once per device.
- **Done when:** `/v1/why?limit=1000` on that queue holds the lock for under 50 ms.

### 27. Give setup its own usage help (done)

- **Why (audit, round 3):** `idlemax setup --coord-url <bad>` prints `help: idlemax job add --help`, because setup reuses the job command's usage helper.
- **Done:** Every setup planning error keeps its full message and points to `idlemax setup --help`.
  A focused dry-run regression covers bad URLs, missing address or token, a non-IP listen address and a conflicting saved address.

### 28. Confirm two platform claims from the audit

- **Why (audit, round 3, unconfirmed):** re-running `idlemax service install` on macOS while a job is stopping may make `bootstrap` fail after an ignored `bootout`; and the OpenCode plugin always goes to `$HOME/.config/opencode` although OpenCode reads `XDG_CONFIG_HOME` and `OPENCODE_CONFIG_DIR`.
- **How:** Reproduce each in a throwaway home; fix only what reproduces.
- **Done when:** Each is either fixed with a test or recorded here as not a bug.

### 29. Keep a Linux machine awake the same way

- **Why (round 4):** `agent.keep_awake = "ac"` holds an idle-sleep assertion only on macOS (`caffeinate -i -w <agent pid>`); a Linux laptop with a short suspend timer still sleeps before the guard's idle wait ends.
- **How:** Hold `systemd-inhibit --what=idle --who=idlemax --why=<reason>` under the same pure decision, as a child that dies with the agent, and reap it on release.
- **Done when:** On a Linux laptop on AC power with a queued job, `systemd-inhibit --list` shows idlemax's inhibitor while the guard waits or runs, and it is gone when the queue empties or the laptop goes on battery.

### 30. Tune the laptop guard defaults from real report data

- **Why (round 4):** On the owner's 16 GiB MacBook, memory pressure `warn` held new work on every sample, and together with the 10 minute user-idle rule it held work for the whole of each test window, so keep-awake correctly refused to hold and no job ran.
  A guard that leaves a machine idle while it has room is a bug by the project goal, but one busy session is too small a sample to change a default.
- **How:** Run `idlemax report` over a few days of normal use, compare the hours held by `memory pressure` and `user active` with the hours the machine was really contended (swap-in rate, PSI, load), and lower the laptop defaults only where the data shows room.
- **Done when:** The laptop preset's memory and idle rules are backed by recorded report numbers in this file, and a day of normal use on that Mac runs queued jobs without the owner noticing them.

## v0.4: learn from real runs and harden

### 11. Learn token costs and tune quota reserves

- **Why:** Token class costs are fixed estimates (SHORTCUT in the scheduler).
  The reserves (40% per 5-hour window, 30% weekly, 15% in the final day) are research estimates.
  idlemax's own AI runs also count as interactive use, which lengthens the hold after every run.
- **How:** After a week of runs, derive per-job usage from the quota snapshots around each AI job, set the reserves from the observed interactive peaks, and leave sessions that idlemax started out of the interactive-use check.
- **Done when:** The scheduler uses measured costs and the SHORTCUT is gone, and a week of night runs never pushes interactive use into a provider limit.

### 20. Catch daemons started from Apple-signed binaries

- **Why:** Item 14 matches a job's processes by group, tree and a random per-attempt environment tag, but macOS hides the environment of a `setsid` daemon started from an Apple-signed binary, so such a daemon is not attributed to its job (SHORTCUT in `agent/runner/os.rs`).
- **How:** Track forks with kqueue `NOTE_TRACK` or Endpoint Security, both heavier than the current poll.
- **Done when:** A daemonized `/usr/bin/perl` memory hog is killed at its job's cap on macOS.

## Later

### 16. Roll out to more devices

- **Why:** Other devices (a Linux desktop, ARM boards, spare laptops) should join by installing one agent.
  There is no Linux desktop user unit yet, and a Linux desktop whose logind never sets the idle hint reports idle 0, which holds every job on purpose.
- **How:** Add a Linux desktop user unit, document the join steps, and decide how a desktop without a logind idle hint measures idle time.
- **Done when:** A third device joins with `idlemax setup --role agent`, shows up in `idlemax status`, and runs a job under its own guard.

### 17. Support Windows

- **Why:** Windows code paths were never built, and several parts are SHORTCUTs: it maps to the `linux_server` preset, reports no idle, power or thermal data, measures its largest disk, cannot pause jobs, kills trees with `taskkill`, stops only on Ctrl-C, gets no token file ACL check, reads `/dev/urandom` for tokens, has no service detection in `doctor`, no session hooks, and the advisor feed reads only `HOME`.
- **How:** A `windows_desktop` preset, `GetLastInputInfo` idle and `GetSystemPowerStatus` power, the system drive, job objects for pause and tree kill, service stop events, a token file ACL check, `BCryptGenRandom`, a `USERPROFILE` fallback, and Task Scheduler packaging after checking each primitive against Microsoft's documentation.
- **Done when:** A Windows device passes the same guard, runner and end-to-end tests as macOS, and every Windows SHORTCUT is gone.
