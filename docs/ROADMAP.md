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
