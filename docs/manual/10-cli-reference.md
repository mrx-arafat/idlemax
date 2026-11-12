# 10. CLI reference 📚

**What you will learn:** every `idlemax` command and every flag, as `--help` prints them for version 0.2.0, with a short example for each.

`idlemax <command> --help` always has the last word for the version you run.

## Global flags

These work on every command, before or after it.

| Flag | Meaning |
|---|---|
| `--coord <URL>` | coordinator URL [default: `coordinator.url` in config.toml] [env: `IDLEMAX_COORD`] |
| `--format <FORMAT>` | `table`, `toon` or `json` [default: table on a terminal, toon when piped] |
| `--json` | same as `--format json` |
| `--utc` | show event times in UTC instead of this machine's local time |
| `--full` | do not cut long text in toon output |
| `--fields <FIELDS>` | add list columns to toon output, e.g. `--fields cost,age` (or `all`) |
| `-V`, `--version` | print the version |
| `-h`, `--help` | print help |

Exit codes: 0 success (including "nothing to do"), 1 runtime error, 2 usage error.
`idlemax job add --wait` has its own codes ([below](#job-add)).

## At a glance

| Area | Commands |
|---|---|
| Set up and run | [`setup`](#setup), [`init`](#init), [`coord`](#coord), [`agent`](#agent), [`service`](#service), [`doctor`](#doctor) |
| Look | [`idlemax`](#idlemax-with-no-command), [`status`](#status), [`jobs`](#jobs), [`why`](#why), [`logs`](#logs), [`inbox`](#inbox), [`report`](#report), [`events`](#events), [`quota`](#quota) |
| Jobs | [`job add`](#job-add), [`job cancel`](#job-cancel), [`schedule`](#schedule) |
| Control | [`pause`](#pause-resume-stop-all), [`resume`](#pause-resume-stop-all), [`stop-all`](#pause-resume-stop-all), [`local`](#local) |
| Dispatcher | [`project`](#project), [`device`](#device), [`mode`](#mode), [`plan`](#plan), [`stop`](#stop), [`start`](#start), [`review`](#review) |
| Settings | [`config`](#config), [`statusline`](#statusline) |
| Agents and UIs | [`tui`](#tui), [`mcp`](#mcp), [`hooks`](#hooks), [`skill`](#skill), [`advise`](#advise) |

---

## idlemax (with no command)

Prints live state: each device with its guard verdict, job counts, open advice, spendable AI accounts, and the next commands that fit.
Waits at most 3 seconds for the coordinator and exits 0.

```sh
idlemax
```

## setup

Set this device up in one command: init, the background service, then doctor (safe to repeat).

```text
idlemax setup [OPTIONS]
```

| Flag | Meaning |
|---|---|
| `--role <ROLE>` | `agent` runs jobs for a coordinator elsewhere, `coord` holds the queue, `both` does everything on one machine [default: both] |
| `--coord-url <URL>` | address agents and the CLI use to reach the coordinator, e.g. `http://100.64.0.1:7878` (required for `--role agent`; a coordinator listens there too) |
| `--token-stdin` | read the coordinator's token from stdin instead of generating one (required for `--role agent`) |
| `--statusline` | also wire Claude Code's statusline into the quota probe (agent and both) |
| `--no-service` | do not install the background service |
| `--dry-run` | print every file and command without writing or running anything |
| `--allow <PROGRAMS>` | programs jobs may run, replacing the allowlist while preserving config comments |
| `--profile <PROFILE>` | guard preset: `laptop`, `workbox` or `production` [default: from the OS] |

```sh
idlemax setup --dry-run --allow git,cargo
```

## init

Create `config.toml` and the API token in `$IDLEMAX_HOME` (never overwrites).

| Flag | Meaning |
|---|---|
| `--profile <PROFILE>` | guard preset: `laptop`, `workbox` or `production` [default: from the OS] |
| `--coord-url <URL>` | coordinator URL agents and the CLI use, e.g. `http://100.64.0.1:7878` |
| `--token-stdin` | read the shared coordinator token from stdin instead of generating one |

```sh
ssh server 'sudo cat /var/lib/idlemax/token' | idlemax init --coord-url http://100.64.0.10:7878 --token-stdin
```

When a config already exists, `init` keeps it and refuses `--coord-url` and `--profile`.

## coord

Run the coordinator in the foreground.
No flags of its own.

```sh
idlemax coord
```

## agent

Run the device agent (one per device) in the foreground.
No flags of its own.

```sh
idlemax agent
```

## service

Install or remove the background service (LaunchAgent or systemd user unit).

| Subcommand | Does |
|---|---|
| `service install [agent\|coord]` | write the service file for this OS and start it; running it again restarts it if the file changed [default: agent] |
| `service uninstall [agent\|coord]` | stop the service and delete its file [default: every installed service] |
| `service status [agent\|coord]` | show whether each service is installed, loaded and running |
| `service render [agent\|coord]` | render a Linux root unit offline; nothing is written or started (use `--format table` for raw unit text) |

| Flag | On | Meaning |
|---|---|---|
| `--bin <PATH>` | install, render | absolute path of the idlemax binary the service runs [default: this binary] |
| `--dry-run` | install, uninstall | print the file and the commands without writing or running anything |
| `--cpus <CPUS>` | render | target CPU count; required with `--memory-mib` for off-host rendering |
| `--memory-mib <MEMORY_MIB>` | render | target total RAM in MiB |
| `--config <PATH>` | render | target server config; explicit facts otherwise use server defaults |
| `--state-dir <PATH>` | render | target state folder (explicit facts default to `/var/lib/idlemax`) |

```sh
idlemax service install --dry-run
idlemax service render agent --cpus 4 --memory-mib 4096 --format table
```

## doctor

Check this device's setup; prints one line per check (`ok`, `warn` or `fail`) and the fix for anything wrong.
Read-only.
Exit 1 when any check fails.

```sh
idlemax doctor
```

## status

Cluster summary and devices: per device the slots and RAM in use and why it is not using more, then the dispatcher block and the AI accounts.

```sh
idlemax status --fields guard,jobs
```

## jobs

List jobs.

| Flag | Meaning |
|---|---|
| `--status <STATUS>` | `queued`, `leased`, `running`, `paused`, `succeeded`, `failed`, `killed`, `cancelled`, `parked` |
| `--limit <LIMIT>` | rows to ask for; the coordinator answers at most 1000 [default: 50] |

```sh
idlemax jobs --status parked
```

## job add

Queue a job from a spec file, stdin (`-`) or a command after `--`.

```text
idlemax job add [OPTIONS] [SPEC] [-- <ARGV>...]
```

| Argument or flag | Meaning |
|---|---|
| `[SPEC]` | spec file in TOML, or `-` to read the spec from stdin |
| `[ARGV]...` | the command and its arguments, after `--`; run directly, never through a shell |
| `--name <NAME>` | job name [default: the command's file name] |
| `--cost <COST>` | `cpu_light`, `cpu_heavy`, `io` or `ai`; `ai` needs `--ai-account` [default: cpu_light] |
| `--device <DEVICE>` | run only on this device (repeatable) |
| `--requires <TOOL>` | tool the device must have, such as cargo (repeatable) |
| `--mem <MIB>` | memory the job may use [default: 512] |
| `--max-runtime <SECS>` | longest the job may run before it is killed [default: 1800] |
| `--priority <PRIORITY>` | higher runs first [default: 0] |
| `--ai-account <ACCOUNT>` | AI account to spend; makes this an AI job |
| `--cwd <PATH>` | directory to run in; a relative path is made absolute here, and must exist on the device that runs the job |
| `--key <KEY>` | idempotency key: while a job with this key has not finished, adding it again returns that job |
| `--wait` | wait for the job to finish and print its result; the exit code mirrors the job |
| `--timeout <SECS>` | stop waiting after this long and exit 124; the job stays queued |

With `--wait`: exit 0 when the job succeeded, 1 when it ended any other way, 2 on a usage error, 124 on timeout.

```sh
idlemax job add --wait --timeout 600 --key nightly -- cargo test
```

## job cancel

Cancel a job (kills it if running).

```sh
idlemax job cancel 7
```

## schedule

Repeat a job on an interval, such as every night.

| Subcommand | Does |
|---|---|
| `schedule add <SPEC> [--every <EVERY>]` | repeat the job in a TOML spec file; the guard still runs it only when the machine is idle. `--every`: `30m`, `6h`, `1d` (minimum 1m) [default: `every` in the spec file] |
| `schedule list` | list schedules and what each is waiting for |
| `schedule rm <ID>` | remove a schedule (a job it already queued is left alone) |
| `schedule pause <ID>` | queue no new jobs from a schedule until it is resumed |
| `schedule resume <ID>` | let it queue jobs again |

```sh
idlemax schedule add examples/jobs/kb-refresh.toml --every 12h
```

## why

Explain why a job is not running, and what to change.

```sh
idlemax why 3
```

## logs

Print the job log from this device.

| Flag | Meaning |
|---|---|
| `--tail <TAIL>` | number of lines to show from the end of the log |
| `-f`, `--follow` | follow new log output |

```sh
idlemax logs 3 -f
```

## inbox

List or show job result notes from the inbox (default: `list`).

| Subcommand | Does |
|---|---|
| `inbox list` | list notes in the inbox (default) |
| `inbox show <ID>` | print the note markdown for a job, or `n<id>` for a notice |

```sh
idlemax inbox show n4
```

## report

What idlemax did with the machine, and what stopped it.

| Flag | Meaning |
|---|---|
| `--days <N>` | report on the last N days only, 1 to 30 [default: today and the last 7 days] |

## events

Recent events.

| Flag | Meaning |
|---|---|
| `--limit <LIMIT>` | rows to ask for; the coordinator answers at most 1000 [default: 50] |

## quota

AI account windows, reserve and headroom, then each account's Budget for today.

## pause, resume, stop-all

| Command | Does |
|---|---|
| `pause` | pause jobs everywhere, or on one device, account or job |
| `resume` | resume paused jobs; lifts exactly the scope you name |
| `stop-all` | kill running jobs and hold new ones (kill switch) until you resume |

| Flag | Meaning |
|---|---|
| `--device <DEVICE>` | only this device |
| `--account <ACCOUNT>` | only this AI account |
| `--job <ID>` | only this job |
| `--reason <REASON>` | recorded in the event log |

```sh
idlemax pause --device laptop --reason "presenting"
idlemax resume --device laptop
```

## local

Talk to this device's agent directly; works while the coordinator is down.

| Subcommand | Does |
|---|---|
| `local status` | agent state on this device |
| `local pause` | pause every job on this device |
| `local resume` | resume after a local pause |
| `local stop-all` | kill every job on this device and hold new ones |

## project

The projects the dispatcher may work in; every one is off until you switch it on.

| Subcommand | Does |
|---|---|
| `project list [--device <DEVICE>]` | list the projects idlemax knows |
| `project add <PATH> [--name <NAME>] [--device <DEVICE>]` | add the git checkout at PATH as a project (it starts off); name defaults to the folder name, device to the only device the coordinator knows |
| `project on <NAME> [--device <DEVICE>]` | let the dispatcher plan work in a project |
| `project off <NAME> [--device <DEVICE>]` | take a project out of the dispatcher's pool |
| `project set <NAME> <SETTING> [VALUES]... [-- <ARGV>...] [--device <DEVICE>]` | change one setting: `base`, `priority`, `off-limits`, `core-paths`, or `setup` and `check` (after `--`); lists take several words or commas; no value clears the setting |

```sh
idlemax project set app check -- cargo test -p core
idlemax project set app off-limits secrets/,docs/legal
```

## device

The devices the dispatcher may use; every one is off until you switch it on.

| Subcommand | Does |
|---|---|
| `device list` | list the devices and whether the dispatcher may use each one |
| `device on <ID>` | let the dispatcher use a device |
| `device off <ID>` | stop the dispatcher's work on a device; plain jobs keep running |
| `device credential <ID>` | make the device's own credential, which opens only the agent routes; shown once |

| Flag | On | Meaning |
|---|---|---|
| `--raw` | credential | print the credential alone, for `\| idlemax setup --role agent --token-stdin` |
| `--revoke` | credential | remove the device's credential; its agent is refused from then on |

## mode

Switch the dispatcher `off` (plans nothing), to `propose` (shows what it would launch and launches nothing) or to `auto` (launches the planned workers one at a time).

```sh
idlemax mode propose
```

## plan

See which workers the dispatcher would launch now and why.

| Flag | Meaning |
|---|---|
| `--refresh` | ask for an orchestrator pass now (one `claude -p --model opus` job; it spends quota) |

## stop

Stop every AI worker with its state saved; nothing launches until `idlemax start`.

| Flag | Meaning |
|---|---|
| `--reason <REASON>` | why, for the event log |

## start

Let the workers `idlemax stop` stopped continue their sessions.

## review

See the finished tasks per project, and keep or discard each one.

| Subcommand | Does |
|---|---|
| `review` | list the finished tasks |
| `review keep <RUN> [--merge]` | keep the task's branch and remove its worktree folder; `--merge` also merges the branch into your current branch, when your checkout is clean and the merge has no conflict |
| `review discard <RUN> [--yes]` | delete the task's branch and worktree, after one confirmation; `--yes` does not ask, needed when no terminal is attached |

## config

Show or change settings in `config.toml` and apply them to the running agent.

| Subcommand | Does |
|---|---|
| `config list [--all]` | the settings you changed, or every setting with its default with `--all` (default) |
| `config get <KEY>` | one setting: its value, its default and where the value comes from |
| `config set <KEY> <VALUE>` | change one setting, save it and apply it to the running agent |
| `config unset <KEY>` | remove a setting from config.toml so its default applies again |

`KEY` is `section.name`, such as `guard.require_ac_power`.
`VALUE` is `true`, `40`, a word, or a comma list such as `git,cargo`.

## statusline

Wire the Claude Code statusline into the quota probe.

| Subcommand | Does |
|---|---|
| `statusline install [--dry-run]` | wrap Claude's statusLine command so quota snapshots reach idlemax |
| `statusline uninstall [--dry-run]` | put the original statusLine command back |
| `statusline status` | show how settings.json is wired and whether captures arrive |

`--dry-run` prints the change as a diff without writing anything.

## tui

Interactive dashboard in the terminal.
Keys are in [chapter 7](07-dashboard-tui-mcp.md#every-key).

## mcp

MCP server on stdio for agent CLIs: status, start, stop, project on/off, finished work.

```sh
claude mcp add --scope user idlemax -- idlemax mcp
```

## hooks

Session hooks for coding agents.

| Subcommand | Does |
|---|---|
| `hooks install` | run the summary at the start of every Claude Code, Codex and OpenCode session |
| `hooks uninstall` | remove the summary again; every other hook stays |
| `hooks status` | show where the hook is installed and whether it still points at a working binary |
| `hooks summary` | print the compact summary a session hook loads (always exits 0) |

| Flag | On | Meaning |
|---|---|---|
| `--claude` | install, uninstall, status | Claude Code (`~/.claude/settings.json`) |
| `--codex` | install, uninstall, status | Codex (`~/.codex/hooks.json` and `config.toml`) |
| `--opencode` | install, uninstall, status | OpenCode (`~/.config/opencode/plugins/idlemax.js`) |
| `--dry-run` | install, uninstall | print the change as a diff without writing anything |

## skill

Print the agent skill, or check the committed copy is current.

| Flag | Meaning |
|---|---|
| `--check [<PATH>]` | compare the skill file at PATH (default `skills/idlemax/SKILL.md`) with the generated one and exit 1 when they differ |

## advise

AI advisor recommendations (default: `list`).

| Subcommand | Does |
|---|---|
| `advise list` | list recommendations, open ones first |
| `advise accept <ID>` | queue the recommendation's suggested job |
| `advise dismiss <ID>` | close the recommendation without acting |
| `advise refresh` | re-run the advisor now |

Next: [Troubleshooting](11-troubleshooting.md)
