# 11. Troubleshooting 🩺

**What you will learn:** the first command to run when something is off, and what to do about the error messages you are most likely to meet.

## Rule one: run doctor

```sh
idlemax doctor
```

It checks this device and prints one line per check, `ok`, `warn` or `fail`, with the exact fix under every warn and fail:

```text
idlemax 0.2.0 doctor
ok    home         /Users/you/.idlemax (default)
ok    config       /Users/you/.idlemax/config.toml parses
fail  token        /Users/you/.idlemax/token is mode 0644: group and others can read it
                   fix: chmod 600 /Users/you/.idlemax/token
warn  coordinator  http://127.0.0.1:7878 is up; the token was not checked
                   fix: resolve the token check above, then run `idlemax doctor` again
...
11 checks: 8 ok, 2 warn, 1 fail
```

- The checks cover the state folder, the config (unknown keys are named), the token, the coordinator (reachable, accepts the token, same version), the agent, the kill switch and local pause, the allowlist, disk, AI accounts, agent CLI logins, `gh`, and the background service.
- It is read-only: it never runs `launchctl` or `systemctl` and starts nothing.
- Exit code 1 when any check fails; a warn alone does not fail.
- It checks **this** device only, so run it on each one.
  `--coord URL` checks against another coordinator.

## Rule two: ask why

| Question | Command |
|---|---|
| Why is this job not running? | `idlemax why <job-id>` |
| Why is this device not doing more? | `idlemax status` (the `WHY` column) |
| Why is no worker starting? | `idlemax plan` |
| Why can't this account be spent? | `idlemax quota` (the `WHY` columns) |
| What happened recently? | `idlemax events` |

## Common problems

### "coordinator unreachable"

```text
error: coordinator unreachable at http://127.0.0.1:7878
help: idlemax coord (or pass --coord <URL> for a running one); `idlemax local stop-all` works without it
```

The coordinator is not running, or this device points at the wrong address.

- One machine: `idlemax service status`, then `idlemax setup` (safe to repeat) or `idlemax service install coord`.
- Several machines: check `coordinator.url` in this device's `config.toml`, and that the private network is up (`curl -fsS <url>/healthz` prints `ok`).
- The kill switch still works without it: `idlemax local stop-all`, or `touch ~/.idlemax/PAUSE`.

### "IDLEMAX_HOME is set but empty"

```text
error: IDLEMAX_HOME is set but empty
```

A script exported `IDLEMAX_HOME=` with no value.
`unset IDLEMAX_HOME` to use `~/.idlemax`, or set it to a real folder.
This is an error on purpose, so a typo never falls back to your real state.

### My job stays `queued`

Run `idlemax why <id>`.
The usual reasons, with what to do:

| Reason starts with | Do this |
|---|---|
| `"<program>" is not in <device>'s agent.command_allowlist` | `idlemax config set agent.command_allowlist <every program>` on that device |
| `user active` | walk away, or lower `guard.user_idle_min_secs` |
| `on battery` | plug in, or `idlemax config set guard.require_ac_power false` |
| `battery` | charge it, or lower `guard.min_battery_pct` |
| `thermal limited` | let the device cool down, or `idlemax config set guard.pause_on_thermal false` |
| `memory pressure`, `memory free`, `MemAvailable` | wait for memory to recover; the fix names the `[guard]` keys |
| `disk free` | free some disk; the keys are `guard.disk_free_mib_pause` and `guard.disk_free_mib_kill` |
| `kill switch file PAUSE present` | delete the `PAUSE` file in that device's state folder |
| `local pause active` | `idlemax local resume` on that device |
| `all jobs are paused` | `idlemax resume` |
| a missing tool from `requires` | install it on the device, or remove it from the spec |
| AI quota | see `idlemax quota` |

`idlemax job add` also warns you up front when no device allows the command.

### "unknown keys" in doctor

```text
warn  config       unknown keys in /Users/you/.idlemax/config.toml: dispatcher.bogus_key
                   fix: remove or fix them: idlemax ignores unknown keys, so a typo silently does nothing
```

Fix the spelling.
`idlemax config list --all` lists every real key.
`idlemax config set` refuses unknown keys outright.

### A job spec is refused

A spec file with an unknown key stops with exit code 2 and a TOML parse error naming the line, so `mem_mib = 5` does not silently use the default.
Use `mem_mb` and `max_runtime_secs`.
`admission` and `[worker]` are set by the dispatcher only; remove them.

### "--cost ai needs --ai-account"

```text
error: --cost ai needs --ai-account <name> (an account from [[quota.accounts]])
```

Name the account, or drop `--cost ai`.

### "invalid interval"

```text
error: invalid value '1.5h' for '--every <EVERY>': invalid interval "1.5h": write a whole number and one unit (s, m, h or d), for example 90m, 6h or 1d
```

Write `90m`.

### HTTP 421 from the coordinator

The coordinator answers only to IP addresses, `localhost`, the host of `coordinator.url` and the names in `coordinator.allowed_hosts`.
Add the name you use to `coordinator.allowed_hosts` in the coordinator's `config.toml`, or put the coordinator's IP address in `coordinator.url`.
`idlemax doctor` names this fix.

### "this call comes from a job or worker that idlemax started"

```text
error: this call comes from a job or worker that idlemax started (its environment has IDLEMAX_JOB_ID); only the owner can do this, from their own terminal, the dashboard or the TUI
```

A job or worker tried to resume, switch on or change something.
That is blocked on purpose.
Run the command from your own terminal, the dashboard or the TUI.

### `setup --role agent` stops at once

```text
error: role agent needs the coordinator's address: pass --coord-url http://<coordinator>:7878 (to run the coordinator on this device too, use --role both)
```

An agent needs `--coord-url` (not a loopback address) and the token on stdin with `--token-stdin`.
Nothing was written; exit code 2.

Other setup refusals, all before anything is written:

- A `--coord-url` that disagrees with the existing `config.toml`, or a `--token-stdin` token that differs from the existing token file: fix or move the old one first ([chapter 8](08-servers.md#rotating-a-token)).
- With `--role coord` or `--role both`, the `--coord-url` host must be an IP address or `localhost`, because the coordinator binds it.

### The service warns about a cargo `target` folder

```text
warning: the binary is inside a cargo target directory; a rebuild or `cargo clean` would break the service. Install it first (`cargo install --path crates/idlemax`) or pass --bin
```

Install the binary properly ([chapter 1](01-install.md#option-b-build-from-source)), then run `idlemax service install` again.

### The Linux service stops when I log out

User units run while you are logged in.
`loginctl enable-linger $USER` keeps them running from boot.

### Claude accounts are never spendable

Until the first statusline capture lands, Claude accounts are never spendable (fail closed).

```sh
idlemax statusline status
```

If it says "not written yet; open Claude Code so it renders the statusline", open Claude Code once.
If it is not wired, run `idlemax statusline install`.

### No worker ever starts

Run `idlemax plan`.
Go down the [checklist](05-dispatcher.md#the-checklist): device on, project on, `workers.clis` set, an account with a usable Budget on that device, mode `auto`.
Common plan lines:

- `held: device laptop is off: idlemax device on laptop`
- `no orchestrator pass: no device has an open worker slot and a ready task`: no project has a ready task yet (set a `check`, or add a roadmap item with a "Done when" line).
- `base checks do not run on a production device`: that device is `production`; base checks and workers need `workbox` or `laptop`.
- Ready tasks listed as "not judged by the orchestrator": no Claude account can run the orchestrator pass now.

### The dashboard asks for the token again

It keeps the token in that browser's local storage.
Private windows and cleared site data forget it.
Paste it again from `cat ~/.idlemax/token`.

### Logs

| What | Where |
|---|---|
| A job's output | `idlemax logs <id>` on the device that ran it |
| The agent on macOS | `~/.idlemax/logs/` |
| The agent on Linux (user unit) | `journalctl --user -u idlemax-agent` |
| A system unit on a server | `journalctl -u idlemax-agent` and `journalctl -u idlemax-coord` |
| More detail | set `RUST_LOG=debug` for the process |

## Still stuck?

Open an issue with the output of `idlemax doctor` and `idlemax --version`: <https://github.com/mrx-arafat/idlemax/issues/new/choose>.
Remove your token and private paths first.
For a security problem, follow [SECURITY.md](../../SECURITY.md) instead of opening a public issue.

Next: [FAQ](12-faq.md)
