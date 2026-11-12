# 4. Jobs and schedules 📦

**What you will learn:** how to queue a command, write a job spec file, wait for a result, repeat a job every night, read its output, and find out why a job is not running.

## Queue a command

```sh
idlemax job add -- cargo test -p core
```

The command after `--` is **argv**: it runs directly, never through a shell.
For pipes, `&&` or `*` you need a shell yourself: `-- sh -c 'make && make test'` (and `sh` must then be on the allowlist, which allows any code).

> [!IMPORTANT]
> The program (`argv[0]`, here `cargo`) must be in the device's `agent.command_allowlist`, or the job waits forever.
> `idlemax job add` warns you when no device allows it.
> On a terminal the warning goes to stderr:
>
> ```text
> warning: no device allows "cargo" (checked: laptop). On the device that should run it, run `idlemax config set agent.command_allowlist <programs>` (list every program it may run), or the job will wait in the queue.
> ```
>
> When the output is piped, the same text is a quoted `warning:` field on stdout.
>
> `idlemax config set agent.command_allowlist` **replaces** the whole list, so name the programs already on it too: `idlemax config set agent.command_allowlist /bin/echo,cargo`.
> The agent applies it at once, but the coordinator learns the new list only at the next heartbeat (`agent.heartbeat_secs`, 30 s), so `idlemax why` may show the old list for up to half a minute.

### The flags

| Flag | Meaning | Default |
|---|---|---|
| `--name N` | job name | the command's file name |
| `--cost cpu_light\|cpu_heavy\|io\|ai` | kind of load; `ai` needs `--ai-account`; a `production` device (the Linux default) takes only `cpu_light` and `io` | `cpu_light` |
| `--device D` | run only on this device (repeat for more) | any device |
| `--requires X` | a tool the device must have, such as `cargo` (repeatable) | none |
| `--mem MIB` | memory the job may use | 512 |
| `--max-runtime SECS` | longest the job may run before it is killed | 1800 |
| `--priority P` | higher runs first; may be negative | 0 |
| `--ai-account A` | AI account to spend; makes this an AI job | none |
| `--cwd PATH` | folder to run in; a relative path is made absolute here and must exist on the device that runs the job | the device's default |
| `--key K` | idempotency key (see below) | none |
| `--wait` | wait for the job to finish and print its result | off |
| `--timeout SECS` | with `--wait`, stop waiting after this long and exit 124 | no limit |

- Flags that describe the job (`--name` through `--cwd`) cannot be mixed with a spec file or `-`; that is a usage error (exit 2).
  `--key`, `--wait` and `--timeout` work with a spec file and with stdin too.
- Flags cannot set `env`, `stdin`, `on_user_active` or `max_attempts`; use a spec file for those.
- `--cwd .` pairs well with `--device`, when the folder exists on one machine only.

## Wait for the result

```sh
idlemax job add --wait --timeout 600 -- /bin/echo "tests passed"
```

The queued line and progress go to stderr; the result goes to stdout, with the end of the log when the job ran on this device.
A coordinator that is down for up to 2 minutes is waited out.

| Exit code | Meaning |
|---|---|
| 0 | the job succeeded |
| 1 | the job ended any other way (failed, killed, cancelled, parked), or idlemax itself failed |
| 2 | usage error |
| 124 | `--timeout` passed first; the job stays queued and keeps its place |

If every device that could take the job refuses its command (allowlist), `--wait` exits 1 at once with the fix, and the job stays queued.

## Make retries safe with a key

Scripts and AI agents retry.
A key stops a retry from queueing a duplicate.
This example uses `/bin/sleep`, which must be on the allowlist to run; if it is not, the job stays queued, so cancel it afterwards with `idlemax job cancel <id>`:

```text
$ idlemax job add --key pr-42-tests -- /bin/sleep 20 | grep result
result: queued
$ idlemax job add --key pr-42-tests -- /bin/sleep 20 | grep result
result: already queued (no-op)
```

While a job with that key is queued, leased, running or paused, adding it again returns that job and exits 0, even with different flags.
Once it finishes, the key is free again.
A key is 1 to 128 characters.

## Job spec files

For anything you run more than once, write a TOML file:

```toml
name = "repo-health"
command = ["/path/to/examples/scripts/repo-health.sh", "/path/to/repo"]
cost = "cpu_heavy"          # cpu_light (default), cpu_heavy, io or ai
requires = ["cargo", "git"] # tools the device must have on PATH
devices = ["my-laptop"]     # device ids; empty means any device
priority = 3                # higher runs first
mem_mb = 1024               # default 512, counted against the device RAM budget
max_runtime_secs = 1500     # default 1800
on_user_active = "kill"     # pause (default), continue or kill
max_attempts = 3            # default 3: tries for interruptions that are not the job's fault
key = "repo-health-a"       # optional idempotency key
every = "1d"                # optional: used by `schedule add`, ignored by `job add`

[env]
CARGO_BUILD_JOBS = "2"
```

```sh
idlemax job add repo-health.toml
cat repo-health.toml | idlemax job add -      # the same, from stdin
```

Every field:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `name` | text | | the job's name |
| `command` | list of text | | argv; `command[0]` must be allowlisted |
| `cwd` | text | the device's default | folder to run in |
| `env` | table | empty | extra environment variables |
| `stdin` | text | none | text fed to the job's standard input, kept out of argv |
| `requires` | list | `[]` | tools the device must have |
| `devices` | list | `[]` (any) | device ids allowed to run it |
| `priority` | number | 0 | higher runs first |
| `cost` | text | `cpu_light` | `cpu_light`, `cpu_heavy`, `io` or `ai` |
| `mem_mb` | number | 512 | memory cap in MiB |
| `max_runtime_secs` | number | 1800 | runtime cap (awake, unpaused seconds) |
| `ai_account` | text | none | the account an `ai` job spends |
| `token_class` | text | none | `small`, `medium` or `large`, for `ai` jobs |
| `on_user_active` | text | `pause` | `pause`, `continue` or `kill` |
| `max_attempts` | number | 3 | tries for interruptions that are not the job's fault |
| `key` | text | none | idempotency key, 1 to 128 characters |
| `every` | text | none | repeat interval for `schedule add` |

> [!NOTE]
> Unknown keys are rejected with exit code 2, so a typo such as `mem_mib` cannot silently fall back to the default.
> Use `mem_mb` and `max_runtime_secs` in files; `--mem` and `--max-runtime` are the flag names.
> `admission` and a `[worker]` section are set by the dispatcher only, and a spec file that names them is refused.

> [!TIP]
> Use `on_user_active = "kill"` for a job that holds a lock you need (a cargo `target` folder, an index, a git clone).
> A paused job keeps holding its lock, and your own build would wait for it.

### What a job can see

A job starts from a minimal environment, not the agent's.
Only `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TMPDIR`, `IDLEMAX_HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, the `XDG_*` base dirs and `DBUS_SESSION_BUS_ADDRESS` carry over.
Then come the spec's `env`, `IDLEMAX_JOB_ID`, and `IDLEMAX_HOME` set to the agent's state folder.
API keys, tokens and the ssh-agent socket in the agent's environment never reach a job; put what a job needs in its `env`.
Jobs never get the idlemax token, and `env` keys starting with `LD_` or `DYLD_` are refused.
`job add` accepts such a spec, but the agent refuses to start it: the job is retried as "could not start" (`start: job refused: env key "LD_X" ...`) until it runs out of attempts and parks, so remove the key.

### Starter jobs

[examples/jobs/](../../examples/jobs/) holds ready-made specs, backed by scripts in [examples/scripts/](../../examples/scripts/):

| Spec | Runs | Cost |
|---|---|---|
| `kb-refresh.toml` | fast-forward a notes repo and list what changed | `cpu_light` |
| `index-refresh.toml` | `codegraph sync` over a list of repos | `cpu_heavy` |
| `repo-health-project-a.toml` | clippy, tests and `npm audit` for one repo, as an inbox report | `cpu_heavy` |
| `pr-review-drafts-claude.toml`, `pr-review-drafts-codex.toml` | draft reviews of PRs waiting on you; the AI gets the PR text on stdin, no tools, and posts nothing | `ai` |

Their paths (`/path/to/...`) and device id (`my-laptop`) are placeholders; change them before use, and add each script to the allowlist.

> [!WARNING]
> `index-refresh` and `repo-health` are `cpu_heavy`, and the PR drafts are `ai`.
> A `production` device (the Linux default) takes only `cpu_light` and `io` jobs, so those wait forever there.
> Run them on a Mac (`laptop`) or a `workbox`, or change `cost` in your copy.

## What a job's status means

| Status | Meaning |
|---|---|
| `queued` | waiting for a device; `idlemax why <id>` says what for |
| `leased` | a device claimed it and is about to start it |
| `running` | the process is running |
| `paused` | frozen (SIGSTOP) because the machine is busy or you paused it; it resumes by itself |
| `succeeded` | exited 0 |
| `failed` | ended on its own with a non-zero exit, a signal or a crash; final |
| `killed` | idlemax stopped it for its own limits (`mem_mb`, `max_runtime_secs`) or because you ran `stop-all`; final |
| `cancelled` | you ran `idlemax job cancel` |
| `parked` | interrupted `max_attempts` times in a row for reasons that are not its fault; needs you to look and queue it again |

```sh
idlemax jobs                    # the latest 50
idlemax jobs --status failed    # only failed ones
idlemax jobs --limit 200        # more rows (the coordinator answers at most 1000)
idlemax job cancel 7            # cancel, killing it if it runs
```

### Retries

A job's own result is final.
Only an ending that is **not the job's fault** is retried, after a backoff of 2 minutes, then 4, 8 and so on, at most 1 hour:

- **Retried:** the lease was lost or expired (the device went offline, the agent or coordinator restarted), the guard killed the job because you came back or the machine was under pressure, or the agent could not start the process.
- **Not retried:** a non-zero exit, a signal or a crash (`failed`), a kill for its own memory or runtime cap (`killed`), `stop-all` and `job cancel`.
  A failing test suite has given its answer.
- **AI jobs** (`cost = "ai"`) get one attempt: their own failure ends `failed`, and an interruption parks them at once.

To run a failed job again, queue it again.

## Read the output

```sh
idlemax logs 1 --tail 20   # the last 20 lines
idlemax logs 1 --follow    # stream until the job finishes (-f)
idlemax inbox              # one row per finished job, newest first
idlemax inbox show 1       # the result note
```

```text
$ idlemax inbox show 1
# echo: succeeded

- Job: 1 (attempt 1)
- Device: laptop
- Started: 2026-11-13 00:28:45 +06:00
- Finished: 2026-11-13 00:28:46 +06:00
- Run time: 0s (paused time excluded)
- Exit code: 0
- Throttle: systemd-run scope
- Log: /home/you/.idlemax/logs/jobs/1.log
...
```

`logs` reads `$IDLEMAX_HOME/logs/jobs/<id>.log` on this device.
If the job ran elsewhere it tells you (`job 12 ran on server; run this there`).

## Why is my job not running?

Several things can hold a job at once: a pause, the guard, the allowlist, a missing tool, AI quota, a retry delay.
`idlemax why` lists every one that applies right now, each with its fix:

```text
$ idlemax why 3
Job 3 (false) is queued but not starting.

laptop (online, heartbeat 0s ago):
  - "/usr/bin/false" is not in laptop's agent.command_allowlist, so it would refuse the job
    fix: on laptop, run `idlemax config set agent.command_allowlist <programs>` and list "/usr/bin/false" with every program it may already run
```

Reasons that hold on every device come first, then each device's own reasons.
For a finished job it says how it ended, for example `Job 1 (echo) succeeded (exit 0).`
You often do not even have to ask: the dashboard and the TUI show the headline reason on every waiting job.

## Repeat a job: schedules

`idlemax schedule` re-queues a job on an interval, such as a nightly health check.
Each run is an ordinary job, so the guard still picks the moment: "every day" means once a day, whenever the machine is idle.

```sh
idlemax schedule add examples/jobs/repo-health-project-a.toml    # uses `every = "1d"` from the file
idlemax schedule add examples/jobs/kb-refresh.toml --every 12h   # the flag wins over the file
idlemax schedule list
idlemax schedule pause 2
idlemax schedule resume 2
idlemax schedule rm 2
```

```text
$ idlemax schedule list
ID  NAME        EVERY  STATE         LAST RESULT
1   kb-refresh  12h    job 2 queued  -
```

- An interval is a whole number and one unit, `s`, `m`, `h` or `d`, from `1m` to `365d`: write `90m`, not `1.5h`.
- `STATE` is what the schedule waits for: `job 2 queued`, `next in 5h41m`, `due now` or `paused`.
  `LAST RESULT` is how the last finished job ended (`succeeded 4s ago`), or `-`.
- A new schedule queues its first job at once.
  The coordinator checks every 15 s and queues the next one when the previous job has ended and a whole interval has passed since.
- A job still queued, leased, running or paused blocks the next one, so a machine that is never idle gets one job, not a backlog.
  After a coordinator outage you also get one job, not one per missed period.
- A failed, killed or cancelled job does not stop the schedule; `schedule pause` does.
- Names are unique.
  `schedule rm` leaves a job it already queued alone and prints the `idlemax job cancel N` that stops it.
- A job that cannot be queued (for example its AI account left the config) shows as an `error` event in `idlemax events`, and the schedule tries again after the next interval.

> [!NOTE]
> There is no cron-style "at 03:00".
> The interval counts from the end of the last run, and the guard picks the time of day.

## Did it help?

```sh
idlemax report           # today and the last 7 days
idlemax report --days 30 # one period, 1 to 30 days
```

```text
today: 6 jobs ran (<1 job-min); guard held 6 min, all of it by memory pressure; queue was empty 12 min of idle time
  jobs    6 ran: 6 succeeded, 0 failed, 0 killed, 0 cancelled; <1 job-min
  held    6 min of 19 min tracked: memory pressure 6 min, you using the machine 6 min
  idle    jobs running 14 s, queue empty 12 min, work waiting 0 s
  longest 6 min from 11-10 16:44 to 11-10 16:50: held by memory pressure
  quota   no AI accounts are configured
```

- `jobs`: the jobs that ran in the period and how they ended.
- `held`: time the guard held new work, split by reason (one moment can have several reasons, so the parts can add up to more than the total).
- `away` (when not zero): time with no heartbeat, because the machine slept or a daemon was down.
- `idle`: awake time no hold covered, split into jobs running, the queue empty, and work that waited more than two minutes while a device could have taken it.
- `longest`: the longest stretch with nothing running while the machine was awake.
- `quota`: AI windows that reset in the period and what they left unused (an upper bound).

Device time adds up across devices: two devices give 48 hours tracked for a 24-hour period.

## Notices: the only things that interrupt you

Three things write a notice, and nothing else does:

1. **Finished tasks**: dispatcher workers that finished, and plain jobs that succeeded, failed, were killed or parked.
   Tasks that finish within 10 minutes of each other share one message.
2. **Something needs you**: a task, an account out of quota past today, a device over the emergency line.
3. **One summary of the day**, after 09:00.

Each arrives on every device as an inbox note named `<date>-n<id>-<kind>.md`.
`idlemax inbox` lists it with `n<id>` in the JOB column, and `idlemax inbox show n<id>` prints it.
Set `agent.notify = true` to also get a macOS notification for each.

Next: [The dispatcher](05-dispatcher.md)
