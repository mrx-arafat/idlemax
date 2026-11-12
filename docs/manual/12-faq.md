# 12. FAQ ❓

**What you will learn:** short answers to the questions people ask most, each with a link to the full story.

### Will idlemax slow my laptop down while I work?

No: that is a bug if it happens.
On the `laptop` profile, plain jobs wait for 10 minutes of idle time, AC power and a healthy machine, and run at the lowest priority the OS offers.
The first sign of contention pauses them.
See [the guard](03-concepts.md#the-guard).

### Then why do AI workers run while I type?

Workers mostly wait on the network, so they follow the worker lines (memory and disk) instead of your idle time.
Under pressure idlemax stops its own newest worker first and never touches your apps.
See [the worker lines](03-concepts.md#the-worker-lines).

### How heavy is idlemax itself?

Measured over 10 minutes on one idle Apple Silicon Mac with no jobs: the coordinator and the agent each used about 0% CPU and 16 to 18 MB of memory.
Each 30-second sample runs about 3.5 short macOS system probes, about 74 ms in total.
Linux and a busy machine have not been measured.
On a server, the coordinator unit is capped at 256 MiB and 20% of one CPU.

### How do I stop everything right now?

```sh
idlemax stop-all        # kill running jobs, hold new ones
idlemax stop            # stop every AI worker, state saved
touch ~/.idlemax/PAUSE  # this device, no network needed
```

### Does it need the internet?

Plain jobs need only your machines and your private network.
AI workers and AI jobs talk to their provider through the official CLI.
The web dashboard loads nothing remote.

### Can idlemax push my code or open pull requests?

No.
Workers commit locally on their own `idlemax/<task>` branch; every remote gets a dead push address and no credential helper.
Nothing leaves the machine unless you push it yourself after `idlemax review keep`.
See [Safety and privacy](14-safety-and-privacy.md).

### Will it merge into my branch?

Only when you run `idlemax review keep <run> --merge`, and only when your checkout is clean and the merge has no conflict.

### Could my AI account get banned?

The risk is reduced, not zero.
idlemax only runs the official CLIs with your own login, never fans out across accounts, never retries an AI job, and stops after any limit message.
Consumer terms still restrict automated use; metered API keys (`provider = "api"`) are the path providers designate for automation.
See [the ban-risk stance](06-quota-and-budget.md#read-this-first-the-ban-risk-stance).

### Will it use up the quota I need?

It spends the surplus: what would expire unused at the reset, after your usual daily use and a 10% safety margin.
Until it has a week of your numbers it keeps half the week for you, and in an account's first week of Runs it spends at most 10%.
At 60% of a 5-hour window, workers stop.

### Which agent CLIs can be workers?

`workers.clis` takes `claude` and `codex`.
agy and OpenCode accounts can be configured for quota with a daily cap of Runs.

### Do I need Herdr?

No.
With `workers.host = "auto"` (the default) a worker runs in a Herdr pane when Herdr answers, and headless otherwise, with its output in the job log.

### Does idlemax change files in my repositories?

No.
Project settings live in the coordinator's database, base checks run in a separate checkout under `~/.idlemax/checks/`, and workers run in worktrees under `~/.idlemax/worktrees/`.
A worker's branch shows up in your repository's `git worktree list` and branch list, because a worktree belongs to the repository.

### Can I run a job at 03:00 every night?

Not at an exact time.
`idlemax schedule add spec.toml --every 1d` runs it once a day, whenever the machine is idle.

### Why does `job add` say no device allows my command?

The program is not in any device's `agent.command_allowlist`.
Add it on the device that should run it: `idlemax config set agent.command_allowlist git,cargo,<yours>`.

### Can I use it on Windows?

Not yet.
There is no service command or packaging, and the Windows code paths are neither compiled nor run.

### Is the API documented?

Yes: [docs/API.md](../API.md) has every route, wire format and control rule.
Every call except `GET /healthz` and `GET /` needs `Authorization: Bearer <token>`:

```sh
curl -H "Authorization: Bearer $(cat ~/.idlemax/token)" http://127.0.0.1:7878/v1/status
```

### Where are my settings, logs and history?

All in `~/.idlemax` (or `$IDLEMAX_HOME`).
See [the state folder](09-configuration.md#what-is-in-the-state-folder).

### How do I uninstall it?

```sh
idlemax service uninstall
rm ~/.local/bin/idlemax
rm -rf ~/.idlemax   # only if you want your history gone too
```

### What has been tested, and what has not?

Verified on macOS (Apple Silicon): focused tests for the safety-critical rules, the end-to-end test (a coordinator and an agent running one job), live runs in temporary state folders (including `job add --wait`, `why`, `logs`, `inbox`, `schedule`, `statusline`, `hooks` and `setup`), the TUI at 80x24 and 120x40, and the web dashboard in Chrome.
CI runs the full suite on macOS and Ubuntu.

| Area | Limit |
|---|---|
| Linux | verified on one Ubuntu 24.04 host with systemd 255 (`scripts/verify-linux.sh`: kernel signals, capped user scopes, a memory hog killed at `MemoryMax`, a CPU job held to its quota, `systemd-analyze verify` on both units); a production server rollout has not run |
| Service install | `idlemax service install` and `idlemax setup` are checked with `--dry-run` and against a stand-in service manager, but have not driven a real launchd or systemd yet |
| Agent integrations | `idlemax hooks install` ran against temp copies of the Claude Code and Codex files; the OpenCode plugin passes `node --check` and ran under a fake plugin host, but OpenCode itself was not driven; installing the skill with `npx skills add` has not run |
| Windows | code paths are neither compiled nor run; no idle, power or thermal signals, no pause, tree kill through `taskkill`, no packaging |
| AI pass | the advisor's `claude -p` pass has not run end to end, because that spends quota |
| Claude quota | needs `idlemax statusline install`; until the first capture lands, Claude accounts are never spendable |
| Schedules | no cron-style times; they live in `coord.db`, so an older idlemax refuses a database a newer one wrote, and the CLI and TUI should match the coordinator's release |
| macOS memory cap | `taskpolicy -m` is soft; the runner's memory check every 5 s is the real cap, and a job child that daemonizes escapes its sum |
| Runtime limit | counts awake time only, tested with an injected clock; a real sleep test has not run |
| Job updates | journaled before sending and replayed after a crash; a supervisor records the job's exit, so a job that ends while the agent is down is reported with its real result; without the supervisor (its executable gone) the lease expires and the job runs again |
| Dashboard | checked in Chrome only; Safari, Firefox and phones have not been checked |

What comes next, with a "done when" for each item, is in [docs/ROADMAP.md](../ROADMAP.md).

Next: [Glossary](13-glossary.md)
