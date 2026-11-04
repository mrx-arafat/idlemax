# idlemax roadmap

v0.1.0 ships the coordinator, the agent, the CLI, the TUI and the web dashboard, built and tested on macOS.
This roadmap collects every gap found while building v0.1.0, every `SHORTCUT:` marker in `crates/`, and the open items from the design research.
Each item says why it matters, how to do it, and when it counts as done.
Re-harvest the shortcut ledger with `grep -rnE '(#|//) ?SHORTCUT:' crates` before planning a release.

## v0.2: run it for real on the Mac and the server

### 1. Verify the Linux agent on the real server

- **Why:** The Linux metrics path, logind idle detection, PSI and the `systemd-run --scope` caps compile and pass fixture tests, but they have never run on Linux.
  The `systemd-run --user` caps also depend on cgroup delegation, which is untested.
  The systemd units were never linted, because `systemd-analyze` does not exist on macOS.
- **How:** Build the agent on the server inside a capped transient scope and run it in a temporary `IDLEMAX_HOME` against a loopback coordinator.
  Compare every sampled signal with `/proc/meminfo`, `/proc/pressure/*`, `/proc/vmstat` and `/sys/class/thermal`.
  Run one memory-hog job and one CPU-bound job, then lint both units with `systemd-analyze verify`.
- **Done when:** Every Linux signal matches the kernel's own numbers.
  The memory hog is killed at `MemoryMax` and the CPU job stays within its quota.
  `systemd-analyze verify` is clean for the coordinator and agent units.

### 2. Deploy the coordinator on the tailnet

- **Why:** The coordinator has only run on loopback, and production binds the server's tailnet address under the capped unit.
- **How:** Install the coordinator unit with `coordinator.listen` set to the tailnet address.
  Give each device the shared token with `idlemax init --token-stdin`, and point the Mac agent's `coordinator.url` at the server.
- **Done when:** The Mac agent heartbeats to the server over the tailnet for 24 hours without a gap.
  The API and dashboard are unreachable from outside the tailnet.
  The coordinator stays under `MemoryMax=256M` and `CPUQuota=20%` for that whole day.

### 3. Send advisor inputs from the Mac agent

- **Why:** The advisor reads `gh` and the session index on whatever host runs the coordinator, so a server coordinator sees neither (SHORTCUT in `advisor/mod.rs`).
- **How:** Have the agent collect the read-only GitHub summary and the session metadata on its own schedule.
  It sends them to the coordinator, which feeds the newest copy to the rules engine.
- **Done when:** With the coordinator on the server, the PR-waiting and unfinished-session rules fire from data the Mac sent.
  No session message text leaves the Mac, only metadata.

### 4. Install and verify the Claude statusline capture

- **Why:** `packaging/claude-statusline-capture.sh` exists but is not installed, so no Claude snapshot is ever written and Claude accounts stay unspendable (fail closed).
- **How:** Wire the wrapper into the Claude Code statusline setting in front of the existing statusline command, and document the step in the README.
- **Done when:** A Claude account shows a fresh snapshot in `idlemax quota` within a minute of interactive use.
  The visible statusline output is unchanged.

### 5. Persist the agent outbox

- **Why:** Pending job updates live only in memory (at most 1000), so an agent crash loses terminal updates and the coordinator re-runs finished work.
- **How:** Append each update to a file under `agent/` before sending it, drop it once the coordinator acks its sequence number, and replay unacked entries at startup.
- **Done when:** A `SIGKILL` of the agent between a job's exit and its ack, followed by a restart, delivers that terminal update exactly once.

### 6. Check the dashboard in a browser

- **Why:** The dashboard passed static checks (`node --check`, computed contrast, no remote resources), but nobody has looked at it in a browser.
- **How:** Open it once against a coordinator holding sample devices, jobs, quota and advice.
  Check the light and dark themes and phone width, and render advice whose title contains HTML.
- **Done when:** Screenshots of both themes show no layout defects, and every control works.
  HTML in advice text shows as plain text.

### 7. Run one supervised AI pass end to end

- **Why:** The `claude -p` advisor pass and the ingest of its output have never run, because a run spends quota.
- **How:** Allowlist `claude`, list the AI account in `agent.ai_accounts`, and wait until the quota is spendable.
  Then let the advisor queue its pass, watch it run under the guard, and review what it ingests.
- **Done when:** One AI pass produces validated recommendations in `idlemax advise` without a retry.
  Its quota use shows in `idlemax quota`.

### 8. Make the two-write store updates atomic

- **Why:** A control's pause flag and its job rows are written in two transactions, and so are an advice accept's job insert and status change (SHORTCUT in `coord/api/jobs.rs` and `coord/api/advice.rs`).
- **How:** Add one store method per use case that performs both writes in a single transaction.
- **Done when:** A test that fails the second write leaves neither change applied.
  Both SHORTCUT markers are gone.

### 9. Count job runtime while awake only

- **Why:** Max runtime uses the wall clock, so a laptop that sleeps past a job's limit kills the job on wake.
- **How:** Measure runtime with a clock that stops during system sleep, or subtract the sleep spans the agent sees as gaps between samples.
- **Done when:** A job on a Mac that sleeps longer than its limit keeps running after wake.
  It is killed only once its awake runtime passes the limit.

### 10. Small agent, store and example cleanups

- **Why:** A few small gaps remain from v0.1.0.
  `device_info` is read once at startup, so tools installed later need an agent restart.
  The guard reports a local pause as "PAUSE file" because both arrive as one flag.
  `agent/event_loop.rs` is over the 400-line guideline.
  `coord/store.rs` still allows dead code (SHORTCUT).
  The example configs and the README contain one machine's absolute home paths.
- **How:** Refresh `device_info` every hour, pass the local pause to the guard as its own input, and split the event loop by responsibility.
  Remove the store's dead-code allow once every method has a caller.
  Write example paths relative to `$HOME` or as placeholders.
- **Done when:** Each gap above is fixed, and the store SHORTCUT is gone.
  `grep -rnE '/(Users|home)/' examples README.md` finds nothing.

## v0.3: learn from real runs and harden

### 11. Learn token costs and tune quota reserves

- **Why:** Token class costs are fixed estimates (SHORTCUT in `scheduler/mod.rs`).
  The reserves (40% per 5-hour window, 30% weekly, 15% in the final day) are research estimates.
  idlemax's own AI runs also count as interactive use, which lengthens the hold after every run.
- **How:** After a week of runs, derive per-job usage from the quota snapshots around each AI job.
  Then set the reserves from the observed interactive peaks, and leave sessions that idlemax started out of the interactive-use check.
- **Done when:** The scheduler uses measured costs and the SHORTCUT is gone.
  A week of night runs never pushes interactive use into a provider limit.

### 12. Detect agy interactive use

- **Why:** agy sessions are not detected (SHORTCUT in `agent/quota_probe.rs`), so an agy account is never held back while it is in use.
- **How:** Locate agy's session files on a real install and add them to `interactive_use`.
- **Done when:** An interactive agy session holds its account for 30 minutes, and the SHORTCUT is gone.

### 13. Keep the AI pass prompt off argv and away from user hooks

- **Why:** The AI pass prompt (up to about 40 KiB) travels as argv and shows in job listings.
  `claude -p` also still runs the owner's Claude hooks.
- **How:** Pass the prompt on stdin or as a file in the job's state, and run the pass with settings that load no user hooks.
- **Done when:** Job listings show no prompt text, and no user hook fires during an AI pass.

### 14. Catch daemonizing job children

- **Why:** The RSS cap walks the parent tree, so a child that reparents escapes the memory sum (SHORTCUT in `agent/runner/os.rs`).
  On macOS this poll-based check is the only real cap, because `taskpolicy -m` is a soft limit there.
- **How:** Track job processes by process group or session instead of parentage, and poll faster while a job is near its cap.
- **Done when:** A test job that daemonizes a memory hog is killed at its cap on macOS and Linux.
  The SHORTCUT is gone.

### 15. Harden the coordinator

- **Why:** Device offline events reset when the coordinator restarts.
  The HTTP server has no header-read timeout, which is acceptable only while it listens on loopback or the tailnet.
- **How:** Persist each device's last online or offline transition in the store, and add a header-read and request timeout layer.
- **Done when:** A coordinator restart emits no duplicate offline events.
  A client that never finishes its headers is dropped within the timeout.

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
  The research verified only `GetLastInputInfo` against Microsoft's documentation, not the Task Scheduler idle condition or the idle priority class.
- **How:** Add a `windows_desktop` preset, `GetLastInputInfo` idle and `GetSystemPowerStatus` power, and measure the system drive.
  Use job objects for pause and tree kill, handle service stop events, check the token file ACL, and use `BCryptGenRandom`.
  Ship Task Scheduler packaging after checking each primitive against Microsoft's documentation.
- **Done when:** A Windows device passes the same guard, runner and end-to-end tests as macOS.
  Every Windows SHORTCUT is gone.
