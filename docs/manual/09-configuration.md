# 9. Configuration ⚙️

**What you will learn:** where settings live, how to change them safely, which ones apply at once, and every setting idlemax has: its name, default, what it does and when to change it.

This list matches `idlemax config list --all` for version 0.2.0.

## Where settings live

All settings live in one file, `config.toml`, in your state folder (`$IDLEMAX_HOME`, default `~/.idlemax`).
Every key is optional; anything left out uses its default.

## Change a setting without opening the file

```sh
idlemax config                                    # only what you changed
idlemax config list --all                         # every setting, its value, default and source
idlemax config get guard.require_ac_power
idlemax config set guard.require_ac_power false
idlemax config unset guard.require_ac_power       # back to the default
```

```text
$ idlemax config get guard.require_ac_power
guard.require_ac_power = false (set in config.toml, default false)
$ idlemax config set guard.require_ac_power true
guard.require_ac_power: false -> true
agent: applied now, no restart needed
backup: /Users/you/.idlemax/config.toml.idlemax.bak
```

What `set` does for you:

- checks the value the way startup does, and refuses one that would stop the agent from starting;
- keeps your comments;
- saves the previous file as `config.toml.idlemax.bak`;
- asks the running agent to reload, and tells you when a setting is read only at start and which process to restart.

A value is `true`, a number such as `40`, a word, or a comma list such as `git,cargo`.
`set` replaces a list, so name every item.
An unknown key is refused with exit code 2: `error: unknown setting `guard.bogus`; run `idlemax config list --all` to see them`.

In `config list`, `off` means the setting is unset (the check is off, or the value comes from somewhere else, such as the OS), and `auto` means idlemax sizes it to the machine.

### What applies at once

- Every `[guard]` key, `agent.command_allowlist`, `agent.notify` and `agent.keep_awake`: applied to the running agent at once.
- Every `[dispatcher]` key: both daemons re-read them when the file's modification time changes.
- Every `[workers]` key: the coordinator reads them at every launch step and passes what a device needs in the worker job, so a change applies to the next launch.
- Every `[quota]` setting except `quota.accounts`: applied at once.
- Everything else (listen addresses, intervals, the profile, `[[quota.accounts]]`): read at start; restart the process the reply names.

> [!NOTE]
> The dispatcher limits (the `dispatcher.*` lines, the review cap and your quota share) can also be changed from the dashboard's Limits card and the TUI's limits pane.
> They change the file of the coordinator's own machine, so change a device's limits on that device.

> [!NOTE]
> TOML has no null, so a `[guard]` override cannot switch a check off.
> Set a value that never trips instead (for example `disk_free_mib_pause = 0`), or use `idlemax config unset` to return to the preset.

Worked examples: [examples/config.toml](../../examples/config.toml) (a Mac), [examples/config.server.toml](../../examples/config.server.toml) (a production server with the coordinator) and [examples/config.workbox.toml](../../examples/config.workbox.toml) (a work server).

## Coordinator

| Key | Default | What it does | Change it when |
|---|---|---|---|
| `coordinator.listen` | `127.0.0.1:7878` | the address the coordinator binds; `0.0.0.0` and `[::]` are refused | the coordinator moves to a server: use its tailnet IP |
| `coordinator.url` | `http://127.0.0.1:7878` | where agents and the CLI reach the coordinator | on every device that talks to a remote coordinator |
| `coordinator.allowed_hosts` | `[]` | host names it answers to besides IP addresses, `localhost` and the host of `url`; any other `Host` gets HTTP 421 | devices reach it by a MagicDNS name |
| `coordinator.lease_secs` | `120` | a lease not renewed by a heartbeat in this time is re-queued with backoff | rarely |
| `coordinator.offline_after_secs` | `90` | a device is offline after this long without a heartbeat | you raised `agent.heartbeat_secs` a lot |
| `coordinator.metrics_retention_days` | `7` | days of per-minute device metrics (and quota history) kept | you want longer reports |

## Agent

| Key | Default | What it does | Change it when |
|---|---|---|---|
| `agent.device_id` | short lowercase hostname | the device's name, used in job specs' `devices` | the hostname is long or shared |
| `agent.profile` | `laptop` on macOS, `production` elsewhere | the guard preset: `laptop`, `workbox` or `production` | a dedicated work server: `workbox` |
| `agent.listen` | `127.0.0.1:7879` | the local agent API, loopback only | another agent already holds the port |
| `agent.sample_secs` | `30` | how often the guard samples the machine | rarely; workers sample every 5 s by themselves |
| `agent.heartbeat_secs` | `30` | how often the agent reports to the coordinator | rarely |
| `agent.dispatch_inputs_secs` | `60` | how often the agent sends its git repositories, your activity, roadmap items and your changed files of switched-on projects; `0` sends nothing | you do not want the dispatcher to see this device's repos |
| `agent.advisor_inputs_secs` | `900` | how often the agent sends its `gh` summary and session metadata to the advisor; `0` sends nothing | you do not want the advisor |
| `agent.notify` | `false` | also post a macOS notification for each notice; the inbox note is written either way | you want pop-ups |
| `agent.keep_awake` | `off` | `ac` keeps a Mac from idle sleep while eligible work is queued or running on AC power; applies at once | your Mac's sleep timer is shorter than the guard's idle wait |
| `agent.ai_accounts` | `[]` | AI accounts this device may spend | you logged in to an agent CLI on this device |
| `agent.command_allowlist` | `[]` | exact `argv[0]` values a job may run; bare names resolve on the agent's `PATH`; empty denies all | always: list the programs you trust |

> [!TIP]
> With `agent.keep_awake = "ac"`, the display may still sleep.
> The hold releases when work drains, the charger disconnects, or the guard or a pause holds work.
> `idlemax status` and `idlemax why` show a `keep-awake:` line with its state.
> Linux sleep inhibition is not implemented yet.

## Guard

The preset comes from `agent.profile`; each key here overrides one number.
"off" means the check is not used by that preset.

| Key | `laptop` | `workbox` | `production` | What it does |
|---|---|---|---|---|
| `guard.max_jobs` | auto: half the CPUs, at least 2 | auto: all CPUs but one, at least 1 | auto: spare CPUs bounded by RAM slots, at least 1 | most jobs at once; setting it pins the value |
| `guard.ram_budget_mib` | auto: a quarter of RAM, at least 1024 | auto: all RAM but 1024 reserve and 128 agent overhead, at least 512 | auto: a quarter of RAM within a 1536 reserve plus 128, at least 512 | total `mem_mb` of running jobs; setting it pins the value |
| `guard.kill_after_breaches` | 2 | 2 | 2 | kill-level breaches in a row before a kill |
| `guard.resume_after_clear` | 2 | 2 | 2 | clear samples needed before jobs resume |
| `guard.user_idle_min_secs` | 600 | off | off | pause while you were active within this many seconds |
| `guard.require_ac_power` | true | false | false | pause on battery |
| `guard.min_battery_pct` | 40.0 | off | off | pause below this battery level |
| `guard.pause_on_thermal` | true | true | true | pause while the machine is thermally throttled |
| `guard.memory_pressure_pause` | `warn` | off | off | macOS memory pressure level that pauses (`normal`, `warn`, `critical`) |
| `guard.memory_pressure_kill` | `critical` | off | off | macOS memory pressure level that kills |
| `guard.mem_free_pct_pause` | 30.0 | off | off | pause below this percent of free memory |
| `guard.mem_free_pct_kill` | 20.0 | off | off | kill below this percent |
| `guard.mem_available_mib_pause` | off | 1024 | 1024 | pause below this `MemAvailable` (Linux) |
| `guard.mem_available_mib_kill` | off | 768 | 768 | kill below this |
| `guard.swap_growth_pause` | `{ mib = 256, window_secs = 60 }` | off | off | pause when swap grows by `mib` within `window_secs` |
| `guard.swap_growth_kill` | `{ mib = 1024, window_secs = 300 }` | off | off | kill when swap grows this much |
| `guard.swap_used_mib_pause` | off | off | off | pause above this much swap in use |
| `guard.swap_used_mib_kill` | off | off | off | kill above this |
| `guard.swap_in_pages_per_sec_pause` | off | 100.0 | 100.0 | pause when swap-in stays above this |
| `guard.swap_in_sustain_secs` | 60 | 60 | 60 | how long swap-in must stay high |
| `guard.psi_mem_some_pause` | off | 10.0 | 5.0 | pause above this PSI memory `some avg10` (Linux) |
| `guard.psi_mem_full_kill` | off | 5.0 | 1.0 | kill above this PSI memory `full avg10` |
| `guard.psi_io_full_pause` | off | 10.0 | 10.0 | pause above this PSI I/O `full avg10` |
| `guard.psi_io_full_kill` | off | 25.0 | 25.0 | kill above this |
| `guard.load_per_cpu_pause` | 1.0 | off | 0.75 | pause above this load per CPU, outside the jobs |
| `guard.load_per_cpu_kill` | 2.0 | off | 1.5 | kill above this |
| `guard.load_1m_pause` | off | off | off | pause above this raw 1-minute load average |
| `guard.load_1m_kill` | off | off | off | kill above this |
| `guard.disk_free_mib_pause` | 2048 | 2048 | 2048 | below this free disk, start nothing new (running jobs go on) |
| `guard.disk_free_mib_kill` | 1024 | 1024 | 1024 | below this, kill running jobs |

How some of these are measured:

- **Load** is the 1-minute load average minus what the running jobs use themselves, divided by the number of CPUs, so a busy job never pauses itself.
- **Disk** is the least free space of `/`, the state folder and the working folder of each running job.
- **Swap in use** is off on every preset: Linux keeps cold pages in swap for days, so it says nothing about pressure.
- **Capacity:** a 10-core Mac with 16 GiB runs up to 5 jobs and 4 GiB of job memory.
  On Linux, the agent also clamps the RAM budget to its cgroup memory limit minus 128 MiB at startup, and an undetermined limit on a server falls back to 1 job and 512 MiB.
  `status` and `why` show both.

Example: your Mac sleeps on battery anyway, and you want jobs while plugged in only but with less idle wait:

```sh
idlemax config set guard.user_idle_min_secs 300
```

## Quota

Full explanations are in [Quota and the Budget](06-quota-and-budget.md#every-quota-setting).

| Key | Default | What it does |
|---|---|---|
| `quota.owner_reserve_pct` | 50.0 | weekly percent reserved for you until your use is learned |
| `quota.safety_margin_pct` | 10.0 | margin subtracted from the surplus |
| `quota.five_hour_stop_pct` | 60.0 | 5-hour usage at which workers stop |
| `quota.last_day_floor_pct` | 15.0 | weekly floor on the day before the reset |
| `quota.server_only_margin_pct` | 5.0 | weekly percent held for you on an account only servers use |
| `quota.measuring_week_cap_pct` | 10.0 | most an account may spend in its first week of Runs (above 0, at most 100) |
| `quota.seed_small_pct` | 3.0 | estimated cost of an unmeasured small Run |
| `quota.seed_standard_pct` | 8.0 | estimated cost of an unmeasured standard Run |
| `quota.small_task_types` | `["broken_check"]` | task sources that start on the small tier |
| `quota.claude_small_model` | `haiku` | Claude small tier model |
| `quota.claude_standard_model` | `sonnet` | Claude standard tier model |
| `quota.codex_small_model` | unset | Codex small tier model |
| `quota.codex_standard_model` | unset | Codex standard tier model |
| `quota.agy_small_model` | unset | agy small tier model |
| `quota.agy_standard_model` | unset | agy standard tier model |
| `quota.opencode_small_model` | unset | OpenCode small tier model |
| `quota.opencode_standard_model` | unset | OpenCode standard tier model |
| `quota.accounts` | `[]` | the `[[quota.accounts]]` tables; a hand edit |

An account table:

```toml
[[quota.accounts]]
name = "claude-max"              # your name for it
provider = "claude"              # claude, codex, agy, opencode or api
probe = "claude_statusline"      # claude_statusline, codex_sessions or manual
# probe_path = "/somewhere/else" # optional
# daily_cap_runs = 5             # agy and opencode only
windows = [
  { kind = "rolling", minutes = 300, reserve_pct = 40.0 },
  { kind = "weekly", reserve_pct = 30.0, final_day_reserve_pct = 15.0 },
]
```

## Advisor

| Key | Default | What it does | Change it when |
|---|---|---|---|
| `advisor.enabled` | `true` | run the advisor | you do not want recommendations |
| `advisor.ai_account` | unset | the account the advisor's AI pass spends; unset runs the rules only | you want AI recommendations; use another account than the dispatcher's |
| `advisor.auto_accept` | `[]` | template names queued without asking | you trust a template fully |

## Dispatcher

| Key | Default | What it does |
|---|---|---|
| `dispatcher.mode` | `propose` | `off` plans nothing; `propose` shows what it would launch; `auto` launches the planned workers |
| `dispatcher.hold_mem_used_pct` | 90.0 | at this memory used percent, no new worker starts |
| `dispatcher.shed_mem_used_pct` | 95.0 | at this percent, idlemax stops its newest worker |
| `dispatcher.emergency_mem_used_pct` | 98.0 | at this percent, every worker stops |
| `dispatcher.hold_disk_free_mib` | 4096 | below this free disk, no new worker starts |
| `dispatcher.shed_disk_free_mib` | 2048 | below this, idlemax stops its newest worker |
| `dispatcher.emergency_disk_free_mib` | 1024 | below this, every worker stops |
| `dispatcher.swap_in_low_pages_per_sec` | 10.0 | swap-in must stay at or under this for a minute before a worker starts (macOS) |
| `dispatcher.swap_in_high_pages_per_sec` | 100.0 | swap-in above this for a minute sheds a worker (macOS) |
| `dispatcher.worker_mem_mib` | 512 | memory one worker is expected to take; sets how many fit under the hold line |
| `dispatcher.linux_reserve_mib` | the profile's | Linux: memory kept for the system or services, outside what workers may fill |
| `dispatcher.linux_psi_some_hold_pct` | the profile's | Linux: at this PSI memory `some avg10`, no new worker starts |
| `dispatcher.linux_psi_full_shed_pct` | the profile's | Linux: at this PSI memory `full avg10`, idlemax stops its newest worker |

The profile values for the Linux lines:

| Line | `laptop` | `workbox` | `production` |
|---|---|---|---|
| Reserve kept for the system or services | 0 | 1024 MiB | 1536 MiB |
| Share idlemax may fill | no cap | no cap | the guard's `ram_budget_mib`, a quarter of memory |
| Hold at PSI memory `some avg10` | 5 % | 10 % | 5 % |
| Shed at PSI memory `full avg10` | 1 % | 5 % | 1 % |

On macOS memory reads the percent Activity Monitor shows.
On Linux it reads the percent in use of what idlemax may fill, and memory pressure (PSI) takes the place of swap-in.
A reading that cannot be taken only holds, and pressure never raises an emergency.
Linux needs PSI (`psi=1` on the kernel command line where it is off); without it the device holds.

> [!TIP]
> Each worker really takes 0.3 to 0.4 GiB for a Claude Code process.
> If your workers use more (personal MCP servers add more), raise `dispatcher.worker_mem_mib` so fewer start at once.

## Workers

| Key | Default | What it does |
|---|---|---|
| `workers.clis` | `[]` | agent CLIs that may run workers, `claude` or `codex`; empty means no worker starts |
| `workers.reviewers` | `[]` | agent CLIs that may review a high-risk diff, `claude` or `codex` |
| `workers.review_cap` | 3 | finished tasks per project that may wait for your review before idlemax moves on |
| `workers.settle_secs` | 60 | seconds between two worker launches on one device; the launch step also runs this often, between every 1 and every 15 seconds |
| `workers.host` | `auto` | `herdr` (a pane), `headless` (a child of the agent, output in the job log) or `auto` (Herdr when it answers, else headless); `herdr` is refused on a `production` device |
| `workers.sandbox` | `auto` | the OS sandbox around each worker (bubblewrap on Linux): `auto` uses it when `idlemax doctor` says it works and runs with the soft fences only otherwise, `required` refuses to start a worker without it, `off` never uses it; a `production` device always requires it and refuses `off`; read by the agent on that device |
| `workers.claude_token_file` | unset | an absolute path to a file only you can read (mode 600) holding a token from `claude setup-token`; a sandboxed Claude worker signs in with it, through its environment only, and your `~/.claude` is not bound; unset, it reads your credentials file read-only and its token refresh may fail (see [Safety and privacy](14-safety-and-privacy.md)) |
| `workers.sandbox_hosts` | `{}` | per CLI, the hosts a sandboxed worker may reach on port 443, such as `claude = ["api.anthropic.com"]`; a CLI not listed gets idlemax's defaults for it (Claude: `api.anthropic.com`, `platform.claude.com` for the token refresh, `claude.ai`; Codex: `api.openai.com`, `chatgpt.com`, `auth.openai.com`); inside the sandbox there is no DNS, every connection goes through the proxy by name |
| `workers.max_runtime_secs` | 3600 | awake seconds one worker launch may run before it is stopped |
| `workers.blocked_secs` | 120 | seconds a worker may wait for input before it is stopped |
| `workers.review_diff_lines` | 300 | a diff with more added plus removed lines gets an independent review |
| `workers.setup_secs` | 900 | limit for a project's setup command in a worker's worktree |
| `workers.check_secs` | 600 | limit for a project's check command in a worker's worktree |

## Environment variables

| Variable | Meaning |
|---|---|
| `IDLEMAX_HOME` | the state folder (default `~/.idlemax`); set but empty is an error |
| `IDLEMAX_TOKEN` | overrides the token file |
| `IDLEMAX_COORD` | the coordinator URL the CLI uses (same as `--coord`) |
| `RUST_LOG` | the log filter, default `info` |

## What is in the state folder

| Path | Holds |
|---|---|
| `config.toml`, `token` | your settings and the 0600 API token |
| `coord.db` | the coordinator database, mode 0600: jobs, devices, quota ledger, advice, schedules, projects and runs |
| `PAUSE` | the kill switch file (only while you want it) |
| `rules.md` | the orchestrator's judgment rules; written once, yours to edit |
| `.owner-writes` | digests of the `config.toml`, `rules.md` and `token` contents your own commands wrote, so a worker's change to them is caught |
| `checks/<project id>` | the dispatcher's detached checkout of a project's base commit |
| `worktrees/` | the workers' git worktrees |
| `jobs/*.toml` | job templates the advisor may suggest |
| `logs/jobs/<id>.log` | job output |
| `inbox/` | one note per finished job, and the notices |
| `agent/` | the 24 h metrics ring, `outbox.jsonl` (job updates not yet acknowledged), `claude-statusline.json`, `quota-<account>.json` manual usage files |
| `bin/claude-statusline-capture.sh` | the capture script `idlemax statusline install` writes |

Next: [CLI reference](10-cli-reference.md)
