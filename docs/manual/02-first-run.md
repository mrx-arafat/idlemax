# 2. Your first run 🚀

**What you will learn:** how to set up your device with one command, what that command creates, how to run your first job, where your token lives, and how to open the dashboard.

## The two background programs

The `idlemax` binary plays two background roles:

- **The coordinator** (`idlemax coord`) keeps the job queue and the database, and serves the API and the web dashboard.
  You need one for all your machines.
  By default it listens on `127.0.0.1:7878`.
- **The agent** (`idlemax agent`) runs on every device.
  It watches the machine, decides when it is safe to work, and runs jobs.
  It also serves a small local API on `127.0.0.1:7879`.

On a single machine, both run on that machine.
That is the setup this page builds.

```mermaid
flowchart LR
  you["You: CLI, TUI, browser"] --> coord["idlemax coord<br/>127.0.0.1:7878"]
  coord <--> agent["idlemax agent<br/>127.0.0.1:7879"]
  agent --> jobs["your jobs"]
```

## Step 1: preview the setup

`idlemax setup` does everything in one go.
Look before you leap with `--dry-run`, which writes and runs nothing:

```sh
idlemax setup --dry-run --allow /bin/echo
```

On a Mac it prints (trimmed):

```text
dry run: nothing is written or run
idlemax setup, role both
would do      config          coordinator.url http://127.0.0.1:7878, coordinator.listen 127.0.0.1:7878, agent.profile laptop
                              would write: ~/.idlemax/config.toml
                              by hand: idlemax init
would do      token           a new random token, mode 0600
                              would write: ~/.idlemax/token
                              by hand: idlemax init
would do      allowlist       agent.command_allowlist = ["/bin/echo"]
                              would write: ~/.idlemax/config.toml
                              by hand: idlemax config set agent.command_allowlist '["/bin/echo"]'
would do      service coord   new, then started
                              ...
would do      service agent   new, then started
                              ...
skipped       statusline      not asked for: add --statusline
                              by hand: idlemax statusline install
would do      doctor          waits up to 10s for services to connect, then checks the whole setup
                              would run: idlemax doctor
                              by hand: idlemax doctor

dry run only: run the same command without --dry-run to apply it
```

On Linux the profile reads `agent.profile production` and the services are systemd user units.
The `by hand` line changes with your flags: with `--profile` it reads `idlemax init --profile <profile>`, and with `--coord-url` it reads `idlemax init --coord-url <URL>` plus a note about `coordinator.listen`.

> [!NOTE]
> This is the `table` format, the default on a terminal.
> When the output goes into a pipe (`| less`, some IDE terminals), idlemax prints the compact `toon` format instead (`role: both`, `steps[7]{step,status,detail,by_hand}:` ...).
> Add `--format table` to get the layout above anywhere.

Every step says `done`, `already done`, `skipped`, `would do` (in a dry run), `starting` or `FAILED`, and shows the command that does the same by hand.

## Step 2: run it

```sh
idlemax setup --allow /bin/echo
```

Here is what it does, in order:

1. **Creates your state folder**, `~/.idlemax`, with `config.toml` (your settings) and `token` (your secret key, file mode 0600).
   It never overwrites either one.
2. **Writes the allowlist.** `--allow /bin/echo` lets jobs run `/bin/echo`, which the example below uses.
3. **Installs the coordinator and the agent as background services**, so they start again after a reboot or a log-in:
   a LaunchAgent on macOS, a systemd user unit on Linux.
4. **Waits up to 10 seconds** for the agent to reach the coordinator.
5. **Runs `idlemax doctor`**, which checks the whole setup.

Running `idlemax setup` again is safe.
Steps that are already done say `already done`.

> [!IMPORTANT]
> **The allowlist starts empty on purpose.**
> A job may only run a program that is listed in `agent.command_allowlist`.
> With an empty list, no job can start.
> When you are ready for real work, list the programs you trust:
>
> ```sh
> idlemax config set agent.command_allowlist git,cargo,/bin/echo
> ```
>
> `set` replaces the whole list, so name every program.
> The running agent picks it up at once, no restart needed.

> [!WARNING]
> Listing a shell or an interpreter (`sh`, `bash`, `python3`, `node`) allows any code at all, because any code can be passed to it.
> Prefer exact programs and scripts.

### The setup flags

| Flag | What it does |
|---|---|
| `--role both` | a coordinator and an agent on this machine (the default) |
| `--role coord` | the queue only; this machine runs no jobs |
| `--role agent` | runs jobs for a coordinator on another machine; needs `--coord-url` and `--token-stdin` |
| `--coord-url URL` | the address agents and the CLI use to reach the coordinator, such as `http://100.64.0.1:7878`; a coordinator listens there too |
| `--token-stdin` | read the coordinator's token from stdin instead of making a new one |
| `--allow PROGRAMS` | the programs jobs may run, as a comma list; replaces the allowlist and keeps your config comments |
| `--profile laptop\|workbox\|production` | the guard preset (default: `laptop` on macOS, `production` on Linux); see [Concepts](03-concepts.md#profiles) |
| `--statusline` | also wire Claude Code's statusline into the quota probe ([chapter 6](06-quota-and-budget.md)) |
| `--no-service` | write the files but install no service |
| `--dry-run` | print every file and command, change nothing |

Exit code 1 means a step failed (or doctor found a failing check after services were installed).
With `--no-service`, a failing doctor (for example no coordinator answering yet) still exits 0, with `setup is done, but doctor found failing checks`, because nothing is meant to run yet.
Exit code 2 means flags that cannot work together, and nothing was written.

> [!TIP]
> **No user systemd?** On WSL, in a container, or over SSH without lingering, the service step fails.
> Run `idlemax setup --no-service --allow /bin/echo`, then start the two programs yourself as [Doing it by hand](#doing-it-by-hand-optional) shows.

## Step 3: your first job

```sh
idlemax job add --wait --timeout 60 -- /bin/echo "hello from idlemax"
```

Everything after `--` is the command.
It runs directly, never through a shell.
`--wait` waits for the result, and `--timeout 60` stops waiting after a minute.

On an idle machine you see:

```text
job 1 queued: echo
job 1 succeeded: echo
exit code: 0
ran: 1.0s on laptop
log (last 20 lines):
=== idlemax job 1 attempt 1 started 2026-11-08T08:56:30.195+00:00 via taskpolicy ===
hello from idlemax

=== idlemax: succeeded (exit 0) ===
```

On Linux the first log line says `via systemd-run scope` (or `nice/ionice`) instead of `via taskpolicy`.

### "It is just sitting there!"

That is the guard doing its job.
What it waits for depends on the profile (see [Concepts](03-concepts.md#profiles)):

- On a Mac (profile `laptop`), idlemax waits until you have been away from the keyboard for 10 minutes, and until the laptop is on AC power.
  The sample below assumes this profile.
- On Linux the default profile is `production`: there is no idle or AC wait, and a job waits only for machine health: load, memory or I/O pressure, swap-in, heat or low disk.
After 60 seconds `--wait` gives up with exit code 124, and the job keeps its place in the queue.
Ask why:

```sh
idlemax why 1
```

```text
Job 1 (echo) is queued but not starting.

laptop (online, heartbeat 2s ago):
  - user active 12s ago (< 600s) (guard pause)
    fix: wait until the device has been idle longer than guard.user_idle_min_secs
```

Every reason comes with the fix and the `[guard]` setting that controls it.
Walk away for ten minutes and it runs. ☕

## Step 4: look around

```sh
idlemax              # a home view: devices, jobs, advice, and what to try next
idlemax status       # every device, how full it is, and why it is not doing more
idlemax jobs         # the job list
idlemax logs 1       # the job's output
idlemax inbox show 1 # the result note
idlemax report       # what idlemax did today and this week
```

`idlemax status` on a fresh machine looks like this (trimmed):

```text
coordinator 0.2.0 | global: running | jobs: 0 running, 0 queued | advice: 0 open

DEVICE  STATE   SLOTS  RAM       WHY          IDLE  CPU  LOAD  MEM FREE  SWAP  DISK    POWER
laptop  online  0/5    0M/4.0G   queue empty  25m   9%   2.10  41% 5.0G  1.0G  120.0G  ac

dispatcher: propose | laptop: run: ...
no workers
needs you: 0
waiting for review: 0
```

## Your token

`~/.idlemax/token` is a random 64-character key.
Every API call, the dashboard and the CLI use it.

> [!CAUTION]
> Treat the token like an SSH key.
> Anyone who holds it can run any allowlisted program, with any arguments, on your devices.
> Never paste it into a chat, an issue or a screenshot.

## The web dashboard

Open <http://127.0.0.1:7878/> in your browser.
The page asks for the token once.
Copy it with:

```sh
cat ~/.idlemax/token
```

Paste it into the page.
The browser keeps it in its local storage until you press **Forget token**.
The dashboard refreshes every 5 seconds and follows your system's light or dark theme.
[Chapter 7](07-dashboard-tui-mcp.md) walks through every card.

Prefer the terminal? Run `idlemax tui` (press `q` to quit).

## The big red buttons

You can stop everything at any time.
Try them now so you know they work:

```sh
idlemax pause              # hold all jobs everywhere
idlemax resume             # and let them go again
idlemax stop-all --reason demo   # kill running jobs and hold new ones
idlemax resume
```

The ultimate switch works with no network at all:

```sh
touch ~/.idlemax/PAUSE     # every job on this device pauses within seconds
rm ~/.idlemax/PAUSE        # and resumes
```

## Doing it by hand (optional)

`setup` is a wrapper.
These are the pieces underneath, if you ever want them:

```sh
idlemax init                        # config.toml and the token, once; never overwrites
idlemax coord                       # run the coordinator in this terminal
idlemax agent                       # run the agent in another terminal
idlemax service install             # the agent as a background service
idlemax service install coord       # the coordinator as a background service
idlemax service status              # installed? loaded? running?
idlemax doctor                      # check everything
```

Where the service lives:

- **macOS:** `~/Library/LaunchAgents/com.idlemax.agent.plist` (and `com.idlemax.coord.plist`), loaded with `launchctl bootstrap`.
  Logs go to `~/.idlemax/logs/`.
- **Linux:** `~/.config/systemd/user/idlemax-agent.service` (and `idlemax-coord.service`), started with `systemctl --user`.
  Read its log with `journalctl --user -u idlemax-agent`.
  A user unit runs while you are logged in; to run it from boot, run `loginctl enable-linger $USER` yourself.

A service starts with a bare environment, so the service file carries your `PATH` and, when set, `IDLEMAX_HOME`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME`.
If you change one of those, run `idlemax service install` again; it rewrites the file and restarts the service (and the jobs it runs).

<details>
<summary>🧪 Try it in a throwaway folder first, with no service at all</summary>

This leaves your real `~/.idlemax` alone and runs both programs in your shell.
Do it **instead of** Step 2, or keep the ports below: if you already ran `idlemax setup`, your services hold ports 7878 and 7879, so the demo uses 17878 and 17879.
The demo config relaxes the guard so the job can start while you are at the keyboard.

```sh
export IDLEMAX_HOME="$(mktemp -d)"
cat > "$IDLEMAX_HOME/config.toml" <<'EOF'
[coordinator]
listen = "127.0.0.1:17878"
url = "http://127.0.0.1:17878"

[agent]
listen = "127.0.0.1:17879"
sample_secs = 5
heartbeat_secs = 5
command_allowlist = ["/bin/echo"]

[guard]
# Demo only: let jobs start while you are at the keyboard and on battery.
user_idle_min_secs = 0
require_ac_power = false
min_battery_pct = 0.0
EOF
idlemax init
idlemax coord >"$IDLEMAX_HOME/logs/coord.log" 2>&1 &
idlemax agent >"$IDLEMAX_HOME/logs/agent.log" 2>&1 &
sleep 2
idlemax status
idlemax job add --wait --timeout 60 -- /bin/echo "hello from idlemax"
```

If the job stays queued, `idlemax why 1` names the `[guard]` key that holds it.
Clean up, and unset `IDLEMAX_HOME` so later commands do not use the deleted folder:

```sh
kill %1 %2
rm -rf "$IDLEMAX_HOME"
unset IDLEMAX_HOME
```

</details>

Next: [How idlemax thinks](03-concepts.md)
