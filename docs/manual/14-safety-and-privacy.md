# 14. Safety and privacy 🔐

**What you will learn:** what idlemax promises never to do, how it keeps jobs and workers in their lane, what data leaves each device, and, honestly, where its fences end.

## The promise in one picture

```mermaid
flowchart TD
  owner["You and your own sessions"] -->|always win| decide{"Room on this device?"}
  decide -->|guard says run, lines say run| work["Jobs and workers run<br/>at the lowest priority"]
  decide -->|you came back, memory or disk tight| back["Pause, shed or stop<br/>idlemax's own work only"]
  kill["PAUSE file, stop-all, stop"] -->|always| back
```

## What idlemax never does

- **Never gets in your way on purpose.** A job that slows your machine is a bug, and so is a guard that holds work while there is room to spare.
- **Never stops, edits or shares the folder of a session you started.** Under pressure it stops its own newest worker first.
- **Never closes your apps.** It names the biggest memory users so you can decide.
- **Never works in your checkout.** Workers get their own worktree and branch; base checks use a separate detached checkout.
- **Never pushes, opens or merges a pull request, deploys, touches a database, sends a message or spends money** on a worker's behalf.
- **Never merges into your branch** unless you run `idlemax review keep <run> --merge` for that task.
- **Never force-removes a worktree.** git refuses while uncommitted work is left, and nothing is deleted.
- **Never reads or copies an AI login.** It launches only the official CLIs with the login already on that device, and reads `claude auth status --json` (no token) to tell logins apart.
- **Never retries an AI job,** and stops spending an account after any limit message.
- **Never resumes your `idlemax stop` by itself,** also after a reboot.
- **Never binds the coordinator to every interface** (`0.0.0.0` or `[::]` is refused).

## Your controls always work

| Control | Scope | Needs the network? |
|---|---|---|
| `touch $IDLEMAX_HOME/PAUSE` | freezes every running job and worker on that device within seconds, and starts nothing new | no |
| `idlemax local stop-all` | kill every job on this device and hold new ones | no |
| `idlemax stop` | every AI worker, state saved, until `idlemax start` | yes |
| `idlemax stop-all [--device/--account/--job]` | kill running jobs in the scope and hold new ones | yes |
| `idlemax pause [...]` | freeze work in the scope | yes |
| `idlemax device off <id>` | stop a device's workers and dispatcher jobs; plain jobs keep running | yes |

Your own `review keep`, `review discard`, `stop` and every switch work whatever the line state, the guard or a pause says.

## How jobs are fenced

- **The allowlist.** A job runs only an exact program listed in `agent.command_allowlist`.
  Every agent reports its allowlist, so the scheduler never leases a job to a device that would refuse it.
- **No shell.** The command is argv and never goes through a shell.
- **A clean environment.** A job gets a short list of variables, its spec's `env` and `IDLEMAX_JOB_ID`.
  API keys, tokens and the ssh-agent socket in the agent's environment never reach it, and it never gets the idlemax token.
  `LD_*` and `DYLD_*` variables are refused.
- **The lowest priority the OS offers, with caps:**

| OS | Wrapper | Memory cap |
|---|---|---|
| macOS | `taskpolicy -c background -d throttle -b -m <mem_mb> -P kill nice -n 20 <command>` | the runner kills the job when its process tree's memory exceeds `mem_mb` (the `taskpolicy` limit is soft) |
| Linux, as root or with a reachable systemd user manager | `systemd-run [--user] --scope --nice=19 -p CPUQuota=100% -p CPUWeight=10 -p IOWeight=10 -p MemoryMax=<mem_mb>M -p MemorySwapMax=0 -- <command>` | cgroup `MemoryMax` with no swap: a job over its cap is killed and ends as "killed by the N MiB memory cap" |
| Linux, otherwise | `nice -n 19 ionice -c3 <command>` | the runner's memory check, plus the agent unit's cgroup caps |

On Linux, systemd 253 or newer also gets `-p OOMPolicy=kill`, so the whole job dies even when the kernel picks a child of a shell, and systemd 254 or newer gets `--expand-environment=no`, so `${VAR}` and `$$` in a job's argv reach it unchanged.
At agent startup on Linux, the agent's own cgroup memory limit (minus 128 MiB) also caps the job RAM budget.

- **A runtime cap.** The runner kills a job after `max_runtime_secs` (default 1800) of awake, unpaused time; sleep does not count.
- **Profiles.** A `production` server never takes `cpu_heavy` or `ai` jobs.

## How workers are fenced

| Fence | How |
|---|---|
| No personal MCP servers | Claude starts with `--strict-mcp-config`; Codex gets `enabled=false` for each configured server |
| No idlemax token, no credential variables | the pane gets them empty, and a worker whose real environment still holds one is never briefed |
| No untracked files of yours such as `.env` | a fresh worktree holds tracked files only |
| No CI, release, secret or agent-config paths | every project's off-limits paths include `.github/`, `.gitlab-ci.yml`, `*.env`, `secrets/`, `.claude/`, `.mcp.json`, `.husky/`, `.githooks/`, `.git/hooks/`, `.envrc`, `.circleci/`, `.env.*`, `*.pem` and `*.key` (unless you set `default-off-limits off`); the brief names them, no scope ever includes them, and a change there rejects the task |
| No `git push` | every remote and every push URL (any scheme, a path, a remote added later) gets a dead push address, with no credential helper and no askpass helper, for the agent and for the setup and check commands; a worker that drops its environment gets past it, which only an OS sandbox stops (A13) |
| Codex writes and network | Codex's own `workspace-write` sandbox, approval `never` |
| No pull requests, deploys, databases, messages or spending | the brief and `rules.md` say so, and the credential variables are missing |

And a finished task is checked by idlemax, not taken on the worker's word: the baseline checks must still pass, the diff must stay in scope, and a large or core diff gets a review by another provider.

### What a worker can still reach

These fences are **soft**: a worker runs as your own user.

- Credentials on disk or in the keychain (ssh keys, the `gh` login, cloud CLI logins) stay readable to a worker that goes looking.
- The idlemax token file and `coord.db` stay readable too.
- The push block can be undone on purpose.

> [!WARNING]
> Switch on `auto` only after the checks in [docs/WORKER-PROOF.md](../WORKER-PROOF.md) passed for each CLI you list in `workers.clis`.

## Who may switch idlemax on

A job or worker that idlemax started can stop and pause work, but it cannot resume it, reload the agent's settings, switch the dispatcher out of `off`, add, switch on or change a project, change a limit, ask for an orchestrator pass, start the workers you stopped, or keep, merge or discard a finished task, even when it reads the token file.

- The CLI refuses those commands when its environment has `IDLEMAX_JOB_ID`.
- The coordinator and the agent answer 403 when the calling process, or any process above it, carries the marker of this idlemax home.
- `idlemax mcp` gives a job or worker no `start` or `project_on`.

### What this lock does not stop

The lock stops accidents and a launched process that tries the documented switches.
**It is not a sandbox.** It does not stop:

- A write to `config.toml` in the state folder: every `[dispatcher]` key, the mode included, and every quota key except `quota.accounts` apply at once, because the daemons re-read them when the file changes; `[workers]` keys apply at the coordinator's next launch step; `[guard]` keys and the other live agent keys apply at the agent's next reload (which `idlemax config set` asks for); every other key, `quota.accounts` included, applies at the next restart.
- A write to `rules.md`, the database or the token file, or deleting the `PAUSE` file: it is the same OS user. A worker's change to `config.toml`, `rules.md` or the token file that no owner command made is detected when its turn is checked: the Run is rejected and the owner gets a needs-you notice.
- Detaching, where a worker has no systemd scope (macOS, or Linux without a systemd manager): one line such as `( env -u IDLEMAX_JOB_ID curl ... & )` leaves a caller with a clean environment under pid 1. On Linux with a systemd manager a worker runs in a scope (`idlemax-w-…`) and every process in one counts as launched whatever its environment, so detaching does not get past the check; a process the worker gets the owner's own session to start still does.
- A call to a coordinator on another machine: a caller that is not on the coordinator's host is judged by the token alone.
- Acting through your own session: a shell start-up file, a git hook or a service file that your session runs later, or a script that drives your browser.
- Replacing the `idlemax` binary.

## Network and tokens

- One bearer token (32 random bytes as hex, file mode 0600) guards every API call except `/healthz`, `/` and `/dashboard.js` (the dashboard page and its script), and is compared in constant time.
- **Anyone holding the token can run any allowlisted program with any arguments on your devices.** Treat it like an SSH key, and keep allowlists narrow: allowlisting `sh` or `python3` allows any code.
- The coordinator's traffic is plain HTTP, so bind it to a private network such as a tailnet, which encrypts it.
- It answers only to IP addresses, `localhost`, the host of `coordinator.url` and `coordinator.allowed_hosts` (a defence against DNS rebinding).
- The local agent API listens on loopback only.
- A server gets its own credential (`idlemax device credential`), which opens only the agent routes and speaks only for that device, so a server keeps no owner token.

## The dashboard

- One self-contained page: it loads nothing remote, sends a strict Content-Security-Policy, and writes every API string as text, never HTML.
- It keeps the token in the browser's local storage until you press **Forget token**.

## What data leaves a device

| Data | Goes to | Turn it off |
|---|---|---|
| Machine metrics and guard verdicts | your coordinator | (needed) |
| Job results and logs metadata | your coordinator; full logs stay on the device | (needed) |
| `gh` summary and session **metadata** (never message text) | your coordinator, for the advisor | `agent.advisor_inputs_secs = 0` |
| Paths of git repositories, your activity, and for switched-on projects the roadmap items, your changed files, the tracked file list (at most 200) and the start of `AGENTS.md` or `CLAUDE.md` (at most 2,000 characters) | your coordinator, for the dispatcher | `agent.dispatch_inputs_secs = 0` |
| A compact summary of tasks and projects, with their file lists and the start of their conventions file | Anthropic, via your own `claude -p` orchestrator pass | `idlemax mode off` |
| The code a worker works on | the worker's AI provider, via its official CLI | keep projects off |
| Quota readings | read from local CLI files only; never sent to a provider | |

Nothing goes to the idlemax authors.
There is no telemetry.

## Reporting a vulnerability

Report it privately, as [SECURITY.md](../../SECURITY.md) describes, not in a public issue.

Back to: [the manual index](00-index.md)
