---
name: idlemax
description: Use when the user asks what spare capacity their devices have, which background jobs are queued, running or failed, why a job is not running, or how much AI subscription quota can be spent before it resets. Runs idlemax from the shell.
---

# idlemax

Run useful jobs on spare device capacity and spend leftover AI quota before it resets.

`idlemax` with no arguments prints live state: each device with its guard verdict, job counts, open advice, spendable AI accounts and the next commands to try. Start there.

## Commands

- `idlemax status`: see the cluster, and per device the slots and RAM in use and why it is not using more
- `idlemax report`: see what idlemax did today and this week, and what held it back
- `idlemax jobs --status <status>`: list jobs, for example failed or parked
- `idlemax job add <spec.toml>`: queue a job from a TOML spec, or a command after `--`
- `idlemax why <id>`: see why a job is not running and what to change
- `idlemax config set <setting> <value>`: change a setting, such as the guard rules or the program allowlist, with no restart
- `idlemax quota`: see AI account windows, today's budget per account and why
- `idlemax advise`: review the advisor's recommendations
- `idlemax project list`: see the projects idlemax knows and switch one on or off
- `idlemax plan`: see which workers the dispatcher would launch now and why
- `idlemax mode <off|propose|auto>`: switch the dispatcher off, to propose, where it only shows what it would launch, or to auto, where it launches workers
- `idlemax stop`: stop every AI worker with its state saved; nothing launches until idlemax start
- `idlemax start`: let the stopped workers continue their sessions
- `idlemax review`: see each finished task with its verdict, then keep it (--merge to merge) or discard it
- `idlemax pause`: hold jobs everywhere, or on one `--device`, `--account` or `--job`
- `idlemax stop-all`: kill running jobs and hold new ones
- `idlemax doctor`: check this device's setup and get the fix for anything wrong

## Good to know

- Add `--json` to a command for machine-readable output.
- A job runs only when the guard allows it; `idlemax why <id>` says what holds it.
- `idlemax local status` still works when the coordinator is down.
- To see live state at the start of every session, ask the user before running `idlemax hooks install`.

## Not installed?

Install the binary with `curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | sh`, then run `idlemax setup`.
