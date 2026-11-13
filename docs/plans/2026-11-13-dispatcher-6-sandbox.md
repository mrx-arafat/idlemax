# Dispatcher 6: Worker Sandbox Implementation Plan

Created: 2026-11-13
Agent: Claude Code
Status: PENDING
Approved: Yes - decided by the orchestrator under the owner's delegation
Iterations: 1
Worktree: Yes (`fix-sandbox`)
Type: Feature

## Summary

**Goal:** A worker that idlemax launches runs inside an OS sandbox by default where one is available, so D6 and D22 hold even when the worker ignores its brief: it can write only its worktree, its own private git dir and a temp dir; it sees none of the owner's home except an allow-list of toolchains, `~/.gitconfig`, the repository's git objects and its CLI's install folder (read-only), and the CLI's own auth files; and it reaches the network only through an allow-list proxy that opens the model API hosts.
The soft fences of Spec 2 (the push block, the blanked credential names, the caller check, `--strict-mcp-config`) stay as a second layer.

This plan answers A13 (Q22) as the orchestrator decided it under the owner's delegation on 2026-11-13, and closes the red-team cases 1 (push), 2 (token and owner routes), 4 (files outside the worktree), 5 (code execution through the shared `.git`), 10 (a process left after stop) and 11 (data off the machine) of `~/idlemax-wt/reports/redteam.md` for a sandboxed worker.

## Out of Scope

- The scope-check bypass of red-team case 6 (the check reads `HEAD`, the merge takes the branch) and the hardening of idlemax's own git calls (`core.fsmonitor`, `core.hooksPath`): separate fixes in Spec 2's code, needed with or without a sandbox, because idlemax's own git runs outside it.
- Running workers as a second OS user: the sandbox gives the same file and network limits without an account the owner has to create and keep in sync.
- The orchestrator pass, setup and check jobs: they are dispatcher jobs that run the owner's own commands, not workers (A16); they keep the guard and the runner's throttles.
- Reviewers (`reviewer_args`): Codex already runs read-only with `--ignore-user-config`; a sandboxed Claude reviewer is a follow-up once the worker sandbox is proven.
- Windows.

## Decisions taken in this spec

1. **Default `auto`, `required` on production.**
   `workers.sandbox` takes `auto` (the default), `required` and `off`.
   `auto` wraps every worker when the sandbox probe passes, and launches with the soft fences only when it does not, saying so as the Run's `sandbox` fact (sent with every update of the attempt, since the coordinator drops `error` of a running update), one event, `idlemax review`, the dashboard, a needs-you line for the device, and `idlemax doctor`; `off` is said the same way (review findings 8 and 10).
   `required` refuses the launch with the probe's reason.
   A `production` device treats `auto` as `required`, and `Config::validate` refuses `off` beside `agent.profile = "production"` (as it refuses `workers.host = "herdr"` there).
2. **Linux: bubblewrap only.**
   The brief named Claude Code's own sandbox settings as the fallback where bwrap is missing, but on Linux Claude Code's sandbox is itself built on bubblewrap (and `socat`), so it is never available where bwrap is not.
   The Linux fallback is therefore "no OS sandbox": `auto` runs soft-fenced with the warning and `required` refuses.
   Claude Code's sandbox settings stay a possible second layer inside bwrap, not a fallback; this plan does not turn them on (Open Question 2).
3. **macOS: `sandbox-exec` with a generated Seatbelt profile**, the same rules as Linux (Task 7).
   It is specified here and built on a Mac; this run builds the Linux part only.
4. **Network through an allow-list proxy.**
   The sandbox has its own network namespace with only loopback.
   The agent serves an HTTP `CONNECT` proxy on a Unix socket; the socket is the only thing of the state dir bound into the sandbox, and a small relay inside the sandbox (`idlemax sandbox-relay`) listens on `127.0.0.1:<port>` and forwards to it.
   The worker gets `HTTPS_PROXY` and `HTTP_PROXY` pointing at the relay.
   The proxy opens `CONNECT host:443` only for the hosts of the worker's CLI in `workers.sandbox_hosts` and refuses everything else with `403`.
5. **The Claude home inside the sandbox is idlemax's, not the owner's.**
   `CLAUDE_CONFIG_DIR` points at `$IDLEMAX_HOME/sandbox/claude`, bound read-write into the sandbox.
   At each launch idlemax writes its `.claude.json` from the owner's one with only the keys the CLI needs to run (`oauthAccount`, `userID`, `hasCompletedOnboarding`, onboarding and theme flags) and the worktree marked trusted, so no trust dialog appears in a sandboxed start.
   The owner's `mcpServers` and `projects` never enter it.
   A Claude worker signs in with the owner's worker token when `workers.claude_token_file` names one (a mode-600 file from `claude setup-token`): the token reaches the CLI only through bwrap's inherited environment as `CLAUDE_CODE_OAUTH_TOKEN` (the headless host sets it on the spawned process, a pane's shim reads the file when it runs), never an argument or a log, and nothing of the owner's `~/.claude` is bound. Without it, the owner's `.credentials.json` is bound read-only and the Run carries the note `Claude token refresh may fail inside the sandbox; set workers.claude_token_file`; `idlemax doctor` says which one is in use (review finding 6, decided by the orchestrator under the owner's delegation).
   Codex gets the same: `CODEX_HOME` points at `$IDLEMAX_HOME/sandbox/codex/<run>`, kept per Run so `codex resume` finds its sessions, with a `config.toml` written from the owner's without `mcp_servers` and a copy of the owner's `auth.json` made at each launch; the owner's file is never bound, so a worker cannot change the owner's Codex login (review findings 5 and N3).
6. **The worker's git is private.**
   Inside the sandbox `GIT_DIR` is a folder of idlemax's own (`$IDLEMAX_HOME/sandbox/git/<run>`) with its own refs, index and objects, and the owner's objects as a read-only alternate; its `config`, `hooks` and `objects/info` are bound read-only.
   No ref, log or `packed-refs` of the owner's repository is writable (review finding 1).
   When the turn ends, idlemax fetches only the Run's branch from it into the owner's repository (the fetch checks every object), so that branch is the only ref a worker can move.
   `scripts/sandbox-live.sh` runs the live attack tests against a real bwrap in a throwaway container.
7. **One process tree.**
   bwrap runs with `--unshare-pid --die-with-parent`, so when the stop kills bwrap every process of the worker dies with its PID namespace, a `setsid` daemon included (red-team case 10).
   Both hosts add `--new-session`, so a pane's CLI cannot push keystrokes into the pane's shell with `TIOCSTI` (review finding 9); the pane TUI then has no controlling terminal, and the proof checks that it still resizes and takes Ctrl-C.
8. **The pane host wraps through a shim.**
   `herdr agent start --kind claude` runs the CLI by its name, so the launch writes a shim directory with one script named after the CLI that `exec`s bwrap with the real program, and puts it first in the pane's `PATH` env pair.
   After the start the launch checks that the agent's process runs in another mount namespace than the agent; when the sandbox was meant to apply and it does not, the launch fails closed.

## Approach

**Chosen:** A pure description of the sandbox in `idlemax_core::worker::sandbox` (the decision, the bwrap arguments, the Claude home and the host allow-list), adapters in `crates/idlemax/src/agent/worker/sandbox/` that probe bwrap, prepare the Claude home, serve the proxy and wrap a `Launch`, and both hosts taking the wrapped program from the `Launch`.
**Why:** The rules live in one pure place and are unit tested without bwrap; the hosts change only in where the program comes from; the proxy is the only long-lived new piece.
The cost is one more process per worker (the relay) and a dependency on bubblewrap that the owner installs.

## Context for Implementer

| What this plan uses | Where it lives |
|---|---|
| `Launch`, `WorkerHost::start` | `crates/idlemax/src/agent/worker/host.rs` |
| The Herdr host, `start_in` | `crates/idlemax/src/agent/worker/herdr.rs` |
| The headless host, `command` | `crates/idlemax/src/agent/worker/headless.rs` |
| The launch phases, `args_for`, `start_checked` | `crates/idlemax/src/agent/worker/steps.rs`, `steps/pane.rs` |
| `agent_args`, `headless_args`, `pane_env`, `pick_host` | `crates/idlemax-core/src/worker/launch.rs` |
| `WorkersConfig`, `Config::validate` | `crates/idlemax-core/src/config/workers.rs`, `crates/idlemax-core/src/config.rs` |
| `Paths` | `crates/idlemax-core/src/paths.rs` |
| Doctor checks | `crates/idlemax/src/doctor/host.rs`, `doctor/mod.rs` |

Measured on the dev server (Ubuntu 24.04, kernel 6.8, 2026-11-13): `bwrap` is not installed, and `kernel.apparmor_restrict_unprivileged_userns = 1` makes `unshare -Ur` fail, so an unconfined bwrap would fail too.
Ubuntu's `bubblewrap` package ships the AppArmor profile that lets bwrap create user namespaces; the doctor fix says to install the package, and the probe runs bwrap for real, so a profile that is missing shows as a failed probe with bwrap's own message.

## File Structure

- `crates/idlemax-core/src/config/workers.rs` (modify) - `workers.sandbox`, `workers.sandbox_hosts`.
- `crates/idlemax-core/src/config.rs` (modify) - refuse `off` on production.
- `crates/idlemax-core/src/worker/sandbox.rs` (create, pure) - `SandboxSetting`, `Probe`, `decide`, the default hosts, `host_allowed`.
- `crates/idlemax-core/src/worker/sandbox/bwrap.rs` (create, pure) - `Fence`, `bwrap_args`.
- `crates/idlemax-core/src/worker/sandbox/claude_home.rs` (create, pure) - `sandbox_claude_json`.
- `crates/idlemax/src/agent/worker/sandbox/mod.rs` (create, adapter) - probe, fence facts, wrap a `Launch`, the mount-namespace check.
- `crates/idlemax/src/agent/worker/sandbox/proxy.rs` (create, adapter) - the allow-list `CONNECT` proxy on a Unix socket.
- `crates/idlemax/src/agent/worker/sandbox/relay.rs` (create, adapter) - the in-sandbox TCP to Unix relay.
- `crates/idlemax/src/cli/...` (modify) - the hidden `sandbox-relay` command.
- `crates/idlemax/src/agent/worker/host.rs`, `herdr.rs`, `headless.rs`, `steps.rs`, `steps/pane.rs` (modify) - the wrapped program.
- `crates/idlemax/src/doctor/host.rs`, `doctor/mod.rs` (modify) - the sandbox check.
- `docs/DISPATCHER.md`, `docs/WORKER-PROOF.md`, `README.md` (modify) - docs.

## Assumptions

- Claude Code honours `HTTPS_PROXY` for its API calls and `CLAUDE_CONFIG_DIR` for its home, and reads `.credentials.json` from there on Linux - Tasks 3 and 4 depend on this; Task 8 proves it with the real CLI.
- Codex honours `HTTPS_PROXY`, and its own sandbox works inside bwrap - Task 4 depends on this; Task 8 proves it.
- Herdr's agent detection works when the CLI runs under bwrap through the shim - Task 4 depends on this; Task 8 proves it, and the launch fails closed when it does not.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A token refresh writes `.credentials.json` with an atomic rename, which fails on a bind-mounted file, so the worker's login ends after a few hours | Medium | Medium | Open Question 1; Task 8 runs a worker past a refresh; the fallback is to bind the owner's `~/.claude` folder's credentials directory instead of the file |
| The pane's shell start-up files rewrite `PATH`, so the shim is skipped and the worker runs bare | Medium | High | The launch checks the agent's mount namespace after the start and fails closed when the sandbox was meant to apply (Task 4) |
| An allow-list too narrow (a telemetry or login host) makes the CLI hang or fail | Medium | Medium | `workers.sandbox_hosts` is a setting; the proxy logs each refused host at info, so the first proof run names what is missing |
| `auto` silently runs bare on a laptop without bwrap | High | Medium | Doctor warns, the Run keeps the `bare` fact with the reason, an event, the review, the dashboard and a needs-you line say it, and `required` is one setting away |

## Goal Verification

### Truths

1. In a sandboxed worker `git push <url>` and `curl https://example.com` fail, while the model API host is reachable through the proxy.
2. In a sandboxed worker `cat $IDLEMAX_HOME/token`, `ls ~/.ssh` and `cat ~/.git-credentials` find nothing, and a write to the owner's checkout or to `.git/hooks` fails.
3. `idlemax stop` leaves no process of a sandboxed worker, a `setsid` daemon included.

## Progress Tracking

- [x] Task 1: Add the sandbox setting and the pure decision
- [x] Task 2: Build the bwrap arguments and the sandbox's Claude home
- [x] Task 3: Serve the allow-list proxy and the in-sandbox relay
- [x] Task 4: Wrap the agent CLI in both hosts and check it took
- [x] Task 5: Report the sandbox in doctor
- [x] Task 6: Document the sandbox
- [ ] Task 7: Needs a Mac: the Seatbelt profile
- [ ] Task 8: Needs the owner's OK: prove the sandbox with bwrap installed

## Implementation Tasks

### Task 1: Add the sandbox setting and the pure decision

**Objective:** One pure function decides, per launch, whether the worker is wrapped, runs bare with a note, or is refused.

**Files:**

- Modify: `crates/idlemax-core/src/config/workers.rs`
- Modify: `crates/idlemax-core/src/config.rs`
- Create: `crates/idlemax-core/src/worker/sandbox.rs`
- Modify: `crates/idlemax-core/src/worker/mod.rs`

**Key Decisions / Notes:**

- `SandboxSetting { Auto, Required, Off }`, serialized snake case, default `Auto`; `workers.sandbox_hosts` is a table from CLI name to hosts, empty by default, which means the defaults of `default_hosts(cli)`: Claude `api.anthropic.com`, `platform.claude.com`, `claude.ai` (review finding 7: Claude Code's network page); Codex `api.openai.com`, `chatgpt.com`, `auth.openai.com`.
- `Probe` is `Usable` or `Unusable(String)` (the reason, such as "bwrap is not installed").
- `decide(setting, profile, probe) -> SandboxUse` returns `Wrap`, `Bare(note)` or `Refuse(reason)`; `Off` gives `Bare("the sandbox is off (workers.sandbox)")`; a production profile reads `Auto` as `Required`.
- `host_allowed(hosts, target)` takes `host:port` and allows only port 443 and an exact host match, case-insensitive, never a suffix.
- Both keys apply live (`plan_reload` lists them under `applied`).

**Definition of Done:**

- [x] `decide` gives `Wrap` for a usable probe in `auto` and `required`, `Bare` with the reason for an unusable probe in `auto`, `Refuse` with the reason in `required` and on a production device in `auto`, and `Bare` for `off`.
- [x] `Config::parse` refuses `workers.sandbox = "off"` beside `agent.profile = "production"` in a line that starts with `workers.sandbox`.
- [x] `host_allowed` allows `api.anthropic.com:443`, refuses `api.anthropic.com:80`, `evil.api.anthropic.com:443` and `api.anthropic.com.evil.io:443`.

**Verify:**

- [x] `cargo test -p idlemax-core sandbox && cargo test -p idlemax-core config::workers`

### Task 2: Build the bwrap arguments and the sandbox's Claude home

**Objective:** Pure functions turn the facts of one launch into the exact bwrap argv and the sandbox's `.claude.json`.

**Files:**

- Create: `crates/idlemax-core/src/worker/sandbox/bwrap.rs`
- Create: `crates/idlemax-core/src/worker/sandbox/claude_home.rs`

**Key Decisions / Notes:**

- `Fence` holds the facts the adapter found: the worktree, the git common dir, the worktree's admin dir (`worktrees/<name>`), the writable git paths that exist, the temp dir, the hidden directories and files that exist (`~/.ssh`, `~/.config/gh`, `~/.gnupg`, `~/.aws`, `~/.docker`, `~/.kube`, `~/.claude`, `$IDLEMAX_HOME`; `~/.git-credentials`, `~/.netrc`, `~/.claude.json`), the Claude home and credentials file when the CLI is Claude, the proxy socket, the relay port, the idlemax binary, the program and its arguments, and whether a new session is wanted.
- The order of `bwrap_args` matters and is pinned by a test: `--ro-bind / /`, `--dev /dev`, `--proc /proc`, `--tmpfs /run`, a tmpfs over each hidden directory, `/dev/null` over each hidden file, then the read-write binds (the worktree, the git paths, the temp dir at `/tmp`, the Claude home, the credentials file), the proxy socket and the idlemax binary under `/run/idlemax/`, the namespaces (`--unshare-net`, `--unshare-pid`, `--unshare-ipc`, `--unshare-uts`, `--die-with-parent`, `--new-session` when asked), the environment (`HTTPS_PROXY`, `HTTP_PROXY`, `https_proxy`, `http_proxy`, `NO_PROXY=` empty, `CLAUDE_CONFIG_DIR` for Claude, `TMPDIR=/tmp`; `--unsetenv` for `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`), `--chdir <worktree>`, then `/bin/sh -c` starting the relay in the background and `exec`ing the program once the relay's ready file exists.
- A hidden path that does not exist is left out (bwrap cannot mount over a missing path on a read-only root).
- `sandbox_claude_json(owner, existing, worktree)` keeps only the allowed keys of the owner's file, keeps the sandbox's own `projects` from the previous launch, and sets `projects[<worktree>].hasTrustDialogAccepted = true`; `mcpServers` never appears.

**Definition of Done:**

- [x] A test pins the full argv for a Claude headless launch, including that the tmpfs over `$IDLEMAX_HOME` comes before the bind of the worktree that lives under it, and that `config` and `hooks` of the common dir are never bound writable.
- [x] A missing hidden path is left out of the argv.
- [x] `sandbox_claude_json` drops `mcpServers` and the owner's `projects`, keeps `oauthAccount`, and marks the worktree trusted.

**Verify:**

- [x] `cargo test -p idlemax-core sandbox`

### Task 3: Serve the allow-list proxy and the in-sandbox relay

**Objective:** The only network path out of the sandbox opens the model hosts and nothing else.

**Files:**

- Create: `crates/idlemax/src/agent/worker/sandbox/proxy.rs`
- Create: `crates/idlemax/src/agent/worker/sandbox/relay.rs`
- Modify: the CLI command table (a hidden `sandbox-relay` command)

**Key Decisions / Notes:**

- The proxy listens on `$IDLEMAX_HOME/sandbox/proxy-<job>-<attempt>.sock` (mode 0600), one per sandboxed worker, with that worker's host list; it reads one request head of at most 8 KiB, answers `CONNECT host:port` with `200` and splices the bytes when `host_allowed`, and `403` otherwise; any other method gets `405`.
  It is started by the wrap step before the CLI, and ends when the worker's host reports the worker gone or the socket is removed at the end of the launch.
- The relay (`idlemax sandbox-relay --socket <path> --port <n> --ready <file>`) listens on `127.0.0.1:<n>`, touches the ready file, and forwards each connection to the socket; it holds no secret and gives nothing the proxy would refuse.
- Each refused host is logged at info with the job id, never the bytes.

**Definition of Done:**

- [x] Over a real Unix socket, `CONNECT` to an allowed host is spliced to a local test server (the test maps the allowed name to a loopback port through an injectable resolver), `CONNECT` to another host gets `403` and `GET` gets `405`.
- [x] The relay forwards a byte stream both ways between a TCP client and the socket.

**Verify:**

- [x] `cargo test -p idlemax --bin idlemax sandbox`

### Task 4: Wrap the agent CLI in both hosts and check it took

**Objective:** Every worker start goes through `decide`; a wrapped one runs under bwrap in both hosts, and a launch whose sandbox did not take fails closed.

**Files:**

- Create: `crates/idlemax/src/agent/worker/sandbox/mod.rs`
- Modify: `crates/idlemax/src/agent/worker/host.rs`, `herdr.rs`, `headless.rs`, `steps.rs`, `steps/pane.rs`

**Key Decisions / Notes:**

- `Launch` gains `wrap: Option<Wrap>` with the program to run instead of the CLI and its leading arguments; the headless host spawns it instead of the CLI, and the Herdr host writes the shim `$IDLEMAX_HOME/sandbox/shims/<job>-<attempt>/<cli>` and adds `PATH=<shim dir>:<agent PATH>` to the pane's env pairs.
- The probe runs `bwrap --ro-bind / / --unshare-net --unshare-pid --die-with-parent /bin/true` with a 5 s limit; its stderr is the `Unusable` reason.
- After the start, `start_checked` asks whether any process of the worker runs in another mount namespace than the agent (`/proc/<pid>/ns/mnt`, the pid and its descendants); for `Wrap` a "no" stops the worker and fails the launch with "the sandbox did not take: the pane ran the CLI outside bwrap".
- The note of `Bare` goes into the first job update; `Refuse` fails the launch with the reason and the fix (install `bubblewrap`, or set `workers.sandbox = auto`).

**Definition of Done:**

- [x] With a fake probe, `required` and no bwrap fails the launch with the reason, and `auto` launches bare with the note in the first update.
- [x] A headless launch with `Wrap` spawns the wrapper program with the CLI after it (stub wrapper that records its argv).
- [x] A Herdr launch with `Wrap` puts the shim directory first in the pane's `PATH` pair, and the shim `exec`s the wrapper.
- [x] A `Wrap` launch whose worker runs in the agent's own mount namespace fails closed.

**Verify:**

- [x] `cargo test -p idlemax --bin idlemax agent::worker`

### Task 5: Report the sandbox in doctor

**Objective:** `idlemax doctor` says whether workers on this device run sandboxed and how to get there.

**Files:**

- Modify: `crates/idlemax/src/doctor/host.rs`, `doctor/mod.rs`

**Key Decisions / Notes:**

- `ok` when the probe passes; `warn` in `auto` when it fails (workers run with the soft fences only), with the fix `sudo apt install bubblewrap` (or the probe's own message when bwrap is installed but cannot make a namespace); `fail` in `required` or on production; `ok` with "off" when the owner switched it off; `ok` with the probe's finding when `workers.clis` is empty, since no worker starts there. On macOS the check reports `sandbox-exec` once Task 7 lands, and until then says the Mac sandbox is not built yet.

**Definition of Done:**

- [x] With a fake probe the check is `ok`, `warn` and `fail` in the three cases above.

**Verify:**

- [x] `cargo test -p idlemax --bin idlemax doctor`

### Task 6: Document the sandbox

**Files:** `docs/WORKER-PROOF.md`, `docs/manual/09-configuration.md`, `docs/manual/14-safety-and-privacy.md` (README has no `[workers]` table; the settings reference is the manual), `docs/DISPATCHER.md` (the Spec map row, in the spec commit).

**Definition of Done:**

- [x] WORKER-PROOF gains a "Sandbox" table with the rows of Task 8, and its "Not fenced" paragraph says what the sandbox now fences.
- [x] The configuration reference lists `workers.sandbox` and `workers.sandbox_hosts`, and the safety page says what the sandbox fences and what it does not.

### Task 7: Needs a Mac: the Seatbelt profile

**Objective:** The same rules on macOS with `sandbox-exec -p <profile>`: deny file writes except the worktree, the git paths and the temp dir; deny reads of the hidden paths; deny network except the proxy's loopback port; the agent's proxy is the same.
Built and proven on the owner's Mac; not part of this run.

**Definition of Done:**

- [ ] A pure `seatbelt_profile(fence)` with a pinned test, the probe `sandbox-exec -p '(version 1)(allow default)' /usr/bin/true`, and the WORKER-PROOF rows passed on the Mac.

### Task 8: Needs the owner's OK: prove the sandbox with bwrap installed

**Objective:** Run the red-team cases 1, 2, 4, 5, 10 and 11 against a sandboxed worker in a throwaway home with the stub CLIs, then one real Claude worker on the owner's sample task.
Needs `bubblewrap` installed on the device, which needs the owner's sudo.

**Definition of Done:**

- [ ] In a throwaway home with stub CLIs: push by URL fails, the token file and `~/.ssh` are not readable, a write to the owner's checkout, `~/.claude` and `.git/hooks` fails, a `setsid` daemon is gone after stop, and a `CONNECT` to a host not on the list gets `403`.
- [ ] With the real Claude CLI: the worker reaches `idle`, commits, and keeps working past a token refresh.

## Open Questions

1. **Credentials refresh across a bind mount (review finding 6, answered: the worker token).** If Claude writes `.credentials.json` by an atomic rename, the rename fails on a bound file, and a refresh by the owner outside leaves the sandbox on the old inode. Binding the credentials' directory instead means binding the owner's whole `~/.claude` writable, where a worker could add hooks to `settings.json` that run in the owner's own sessions and read every project's transcripts. Not done; the proposed way is a long-lived token for workers (`claude setup-token`, passed in the worker's environment, never in argv), with the file bind only as a fallback. Needs the owner's choice.
2. **Claude Code's own sandbox inside bwrap.** It would add a per-command layer for the CLI's Bash tool, but needs bwrap inside bwrap; not turned on until proven.
3. **The allow-list.** Claude's hosts are from its network page (code.claude.com/docs/en/network-config, read 2026-11-13): `api.anthropic.com`, `platform.claude.com` (OAuth refresh) and `claude.ai`. Codex's are from its documentation and still to be seen in a real run. The sandbox has no DNS on purpose (`/run` is a tmpfs); a tool that resolves a name before it connects fails, while every CLI call goes through the proxy's `CONNECT` by name.
4. **A CLI installed under a hidden folder.** Claude's "local" install lives in `~/.claude/local`, which the sandbox hides, so the CLI would not start inside it. Task 8 checks where the CLI resolves; the fix is a read-only bind of the CLI's own install folder.
5. **Ubuntu 24.04 and AppArmor.** With `kernel.apparmor_restrict_unprivileged_userns = 1` an unconfined bwrap cannot set up its user namespace, and the 24.04 `apparmor` package ships no profile for it. Installing `bubblewrap` there also needs a profile for `/usr/bin/bwrap` that allows `userns`; the doctor probe shows bwrap's own error until it exists, and the doctor fix should name the profile once Task 8 confirms the exact steps.
6. **Bind sources under hidden folders.** The Claude credentials file sits in `~/.claude`, which is hidden by a tmpfs before the file is bound back. bwrap resolves bind sources against the original root, so this is expected to work; Task 8 confirms it.
