# idlemax roadmap

v0.1.0 ships the coordinator, the agent, the CLI, the TUI and the web dashboard, built and tested on macOS.
This roadmap collects every gap found while building and reviewing it, every `SHORTCUT:` marker in `crates/`, and the open items from the design research.
Each open item says why it matters, how to do it, and when it counts as done.
Item numbers are stable, so commit messages and notes can refer to them.
Re-harvest the shortcut ledger with `grep -rnE '(#|//) ?SHORTCUT:' crates` before planning a release.

## Status after the first v0.2 round (2026-11-06)

### Landed on main

| Item | What changed | Commit |
|---|---|---|
| 5 | Job updates are journaled before sending and replayed after a crash, and the coordinator applies each update once | `e06a009` |
| 3 | Each agent sends the GitHub summary and session metadata to the coordinator, so the advisor works from a server coordinator | `da05dea` |
| 8 | Control and advice-accept writes are atomic, and the store dead-code allow is gone | `b021525` |
| 9 | Job runtime counts awake time only | `22cda05` |
| 10 | Device info refreshes hourly, and the guard names a local pause apart from the PAUSE file | `f034e80` |
| 4, 10 | Statusline capture install step in the README, wrapper proof test, example placeholders instead of one machine's paths | `54e2cdd` |
| 12 | Interactive agy sessions hold their account | `d925e93` |
| 13 | The AI pass prompt travels on stdin, and the pass runs with user hooks and MCP servers off | `9ebc796` |
| 15 | Device presence survives a coordinator restart, and the server has header-read and request timeouts | `7ed16d9` |
| 6 | Dashboard checked in a browser, with layout, refresh and contrast fixes | `02f8744` |
| 4 | The statusline wrapper passes its input through byte for byte | `e780430` |

### Written and tested locally, not reviewed or landed yet

- Item 14 (daemonizing job children): the design, the pure accounting and two live tests exist.
  The new test failed 3 of 6 early runs under heavy load, so it must be made reliable before landing (see item 14).
- Items 1 and 2 (Linux verification and tailnet rollout): `scripts/verify-linux.sh` with a fixture self-test, `docs/ROLLOUT.md`, a battery parser fix and four Linux parser tests exist.
  It also adds `--expand-environment=no` to the systemd scope wrapper, which must be checked on the server before landing (see item 1).

### Needs the owner's OK before it runs

- Running the Linux verification on the real server (item 1) and deploying on the tailnet (item 2).
- Installing the statusline wrapper into the real Claude settings (item 4).
- One supervised AI pass, which spends quota (item 7).
- A real sleep test on the Mac (item 9).

## v0.2: run it for real on the Mac and the server

### 1. Verify the Linux agent on the real server

- **Why:** The Linux metrics path, logind idle detection, PSI and the `systemd-run --scope` caps compile and pass fixture tests, but they have never run on Linux.
  The `systemd-run --user` caps also depend on cgroup delegation, which is untested.
  The systemd units were never linted, because `systemd-analyze` does not exist on macOS.
- **How:** Review and land the local preparation (the verification script, the runbook, the battery parser fix).
  First check that the server's `systemd-run --help` lists `--expand-environment`, because the prepared change makes the scope wrapper pass `--expand-environment=no` and an unknown option would fail every job there.
  Then run the script on the server as the normal deployment user, in a temporary `IDLEMAX_HOME` against a loopback coordinator, with a binary built inside a capped transient scope.
- **Done when:** Every Linux signal matches the kernel's own numbers.
  The memory hog is killed at `MemoryMax` and the CPU job stays within its quota.
  `systemd-analyze verify` is clean for the coordinator and agent units.

### 2. Deploy the coordinator on the tailnet

- **Why:** The coordinator has only run on loopback, and production binds the server's tailnet address under the capped unit.
- **How:** Follow the runbook from item 1: install the coordinator unit with `coordinator.listen` set to the tailnet address, give each device the shared token with `idlemax init --token-stdin`, and point the Mac agent's `coordinator.url` at the server.
  Once, before the 24-hour run, point a v0.1.0 agent at the new coordinator, because old-agent compatibility rests on serde defaults and one API test only.
- **Done when:** The Mac agent heartbeats to the server over the tailnet for 24 hours without a gap.
  The API and dashboard are unreachable from outside the tailnet.
  The coordinator stays under `MemoryMax=256M` and `CPUQuota=20%` for that whole day.

### 4. Install and verify the Claude statusline capture

- **Why:** The wrapper and its README step are done and tested, but it is not installed, so no Claude snapshot is written and Claude accounts stay unspendable (fail closed).
- **How:** Run `idlemax statusline install --dry-run`, check the diff, then `idlemax statusline install` against the real Claude settings.
- **Done when:** A Claude account shows a fresh snapshot in `idlemax quota` within a minute of interactive use.
  The visible statusline output is unchanged.

### 7. Run one supervised AI pass end to end

- **Why:** The `claude -p` advisor pass and the ingest of its output have never run, because a run spends quota.
  The pass now sends its prompt on stdin and runs with `--settings {"disableAllHooks":true} --strict-mcp-config`, and that has only been checked against the documented flags and unit tests, never against a real `claude -p`.
- **How:** Allowlist `claude`, list the AI account in `agent.ai_accounts`, and wait until the quota is spendable.
  Then let the advisor queue its pass, watch it run under the guard, and review what it ingests.
- **Done when:** One AI pass produces validated recommendations in `idlemax advise` without a retry.
  Its quota use shows in `idlemax quota`, and no user hook or MCP server started during the pass.

### 9. Prove the awake-only runtime on a real sleep

- **Why:** Runtime is now measured with the monotonic clock, which does not advance while the machine sleeps on macOS and Linux (confirmed in the Rust standard library source), and an injected-clock test covers the logic.
  A real sleep has not been tried.
- **How:** Start a job with a short `max_runtime_secs`, let the Mac sleep for longer than that, wake it, and watch the job.
- **Done when:** The job is still running after wake and is killed only once its awake runtime passes the limit.

### 10. Split the agent event loop

- **Why:** `agent/event_loop.rs` is 476 lines, over the 400-line guideline, after the outbox, advisor feed and device refresh wiring landed.
- **How:** Split it by responsibility, for example the feed and refresh tickers apart from the heartbeat and job loop.
  Move code only, with no behaviour change.
- **Done when:** `event_loop.rs` is under 400 lines and the agent tests are unchanged and green.

### 14. Catch daemonizing job children

- **Why:** The RSS cap walks the parent tree, so a child that reparents escapes the memory sum (SHORTCUT in `agent/runner/os.rs`).
  On macOS this poll-based check is the only real cap, because `taskpolicy -m` is a soft limit there.
- **How:** Land the local work: a process belongs to a job if it is the root, in the root's process group, carries the job's `IDLEMAX_JOB_ID` environment tag, or descends from one of those, and members outside the group are signalled by pid.
  The poll runs every second while a job is at half its cap or not yet measured.
  Before landing, make the daemon test reliable under load: its early failures show the job's resident size never reaching the cap in 30 seconds, and the unproven guess is that macOS compresses idle pages out of the resident size.
  Also make `pidfile::reap_orphans` kill members outside the group after an agent restart.
- **Done when:** A test job that daemonizes a memory hog is killed at its cap on macOS and Linux, and the test passes repeatedly under load.
  The parent-tree SHORTCUT is replaced by one narrower SHORTCUT: a daemon started from an Apple-signed binary hides its environment tag on macOS, and catching it needs kqueue `NOTE_TRACK` or Endpoint Security.

### 19. Finish the dashboard checks

- **Why:** The browser pass used Chrome only, and the filter's empty state was not exercised.
- **How:** Open the dashboard in Safari and Firefox and on a real phone, and filter jobs by a status that has none.
- **Done when:** No layout or control defect shows in any of them, and an empty filter shows a clear empty state.

## v0.3: learn from real runs and harden

### 11. Learn token costs and tune quota reserves

- **Why:** Token class costs are fixed estimates (SHORTCUT in `scheduler/mod.rs`).
  The reserves (40% per 5-hour window, 30% weekly, 15% in the final day) are research estimates.
  idlemax's own AI runs also count as interactive use, which lengthens the hold after every run.
- **How:** After a week of runs, derive per-job usage from the quota snapshots around each AI job.
  Then set the reserves from the observed interactive peaks, and leave sessions that idlemax started out of the interactive-use check.
- **Done when:** The scheduler uses measured costs and the SHORTCUT is gone.
  A week of night runs never pushes interactive use into a provider limit.

### 18. Close the remaining outbox gaps

- **Why:** The journal covers the common crash, but a few windows still lose work.
  If the agent dies after a job exits and before it is reaped, no update is produced and the lease expiry re-runs the job.
  If the lease expires while the agent is down, the replayed update is ignored and the job re-runs.
  The AI pass output is ingested after the transaction, so a coordinator crash in between loses that one advisor output.
  A failed journal write turns persistence off until restart (SHORTCUT in `agent/outbox.rs`), and the journal is synced with `sync_data`, which survives `kill -9` but not necessarily a power cut on macOS.
- **How:** Write a job-ended marker from something that outlives the agent, accept a replayed terminal update after lease expiry when the job has not run again, ingest the AI output inside the transaction, retry a failed journal write with a rewrite, and use `F_FULLFSYNC` on macOS.
- **Done when:** A `SIGKILL` between a job's exit and its reap still delivers the terminal update once, and the outbox SHORTCUT is gone.

## Later

### 16. Roll out to more devices

- **Why:** v0.1.0 targets one Mac and one server, and other devices (a Linux desktop, ARM boards, spare laptops) should join by installing one agent.
  There is no Linux desktop user unit yet.
  A Linux desktop whose logind never sets the idle hint reports idle 0, which holds every job on purpose.
- **How:** Add a Linux desktop user unit and document the join steps.
  Decide how a desktop without a logind idle hint measures idle time.
- **Done when:** A third device joins with `idlemax init` and its unit, shows up in `idlemax status`, and runs a job under its own guard.

### 17. Support Windows

- **Why:** Windows code paths compile in principle but were never built, and several parts are SHORTCUTs.
  Windows maps to the `linux_server` preset, reports no idle, power or thermal data, and measures its largest disk instead of the system drive.
  It cannot pause jobs, kills process trees with `taskkill`, and stops only on Ctrl-C.
  Its token file gets no ACL check, and token generation reads `/dev/urandom`, which does not exist there.
  The advisor feed reads only `HOME` for the session index, so it finds nothing where only `USERPROFILE` is set.
  The research verified only `GetLastInputInfo` against Microsoft's documentation, not the Task Scheduler idle condition or the idle priority class.
- **How:** Add a `windows_desktop` preset, `GetLastInputInfo` idle and `GetSystemPowerStatus` power, and measure the system drive.
  Use job objects for pause and tree kill, handle service stop events, check the token file ACL, use `BCryptGenRandom`, and fall back to `USERPROFILE` in the advisor feed.
  Ship Task Scheduler packaging after checking each primitive against Microsoft's documentation.
- **Done when:** A Windows device passes the same guard, runner and end-to-end tests as macOS.
  Every Windows SHORTCUT is gone.
