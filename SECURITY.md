# Security Policy

## Supported versions

Security updates are provided only for the latest release.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Older releases | No |

## Reporting a vulnerability

Do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.
Please report vulnerabilities privately using GitHub's "Report a vulnerability" feature under the Security tab of this repository.
Include a description of the issue, the version of idlemax affected, steps to reproduce, and any proof-of-concept code.
Reports will be reviewed promptly, and fixes will be prepared in private before being published in a new release.

## Scope

The following areas are in scope for security reports:

- Token handling: generation of 32-byte hex tokens, file permissions (mode 0600), storage in `$IDLEMAX_HOME/token`, and constant-time comparison.
- HTTP API: token verification across endpoints, request validation, refusal of wildcard bindings (`0.0.0.0`, `[::]`), and loopback enforcement for the local agent API.
- Job execution: process isolation, the minimal environment a job starts with (and the refusal of `LD_*` and `DYLD_*` variables and of the API token), argv execution without shell interpolation, allowlist enforcement, field limits on job specs, and OS resource limits.
- Statusline wrapper: safe capture of usage metrics in `packaging/claude-statusline-capture.sh`, atomic file replacement, avoiding credential exposure, and non-blocking failure semantics.
- Files idlemax writes: the modes of the state directory, job logs, inbox notes and backups, and the config edits made by `idlemax hooks`, `statusline` and `service`.
- The advisor: untrusted text from GitHub and agent sessions, and the confinement of the AI pass.

Third-party dependencies with known vulnerabilities that do not affect idlemax's execution paths are out of scope unless accompanied by an exploitable vector.

## Security model summary

idlemax is built around several core security guarantees.

### Who is trusted

- One bearer token (at least 32 characters, 64 when `idlemax init` makes it, file mode 0600) guards every API call except `/healthz` and `/`, and is compared in constant time.
  Shorter tokens are refused at startup, by `init --token-stdin` and by `doctor`.
- Every device and every CLI shares that one token, so a holder can act as any device: lease its jobs, report results and read every job spec.
  Treat the token like an SSH key; if it leaks, replace it everywhere.
- Anyone holding the token can run any allowlisted program with any arguments and almost any environment on your devices.
  An allowlist entry names a program, not a use of it: `git` allows `git -c core.sshCommand=...`, and a job may set `PATH`, `BASH_ENV` or `NODE_OPTIONS`.
  Keep allowlists narrow, and never allowlist generic interpreters such as `sh` or `python3`.
- There is no per-device credential and no login throttle: the protection is the token's length and keeping the coordinator off untrusted networks.

### Network

- The coordinator refuses to bind `0.0.0.0`, `[::]` or `[::ffff:0.0.0.0]`.
  Its traffic is plain HTTP, so bind it to a private network such as a tailnet that encrypts transit traffic.
- A peer that opens many idle connections cannot stop the coordinator: a failed `accept` is logged and retried, and header timeouts close idle connections.
  It can still crowd out other clients while it does, which is one more reason to keep the coordinator on a private network.
- The local agent API listens on loopback only, needs the same token, and has the same header and request timeouts as the coordinator.
- Browsers cannot call the API with the token: the API sends no CORS headers and answers preflights with 405.
- Every route of the coordinator and of the agent's local API checks the `Host` header and answers 421 for a name it does not serve, so a web page that reaches 127.0.0.1 under its own domain name (DNS rebinding) gets nothing, not even the token-free dashboard page.
  IP addresses, `localhost`, the host of `coordinator.url` and the names in `coordinator.allowed_hosts` are served; a device that reaches the coordinator by a DNS name (a MagicDNS name, say) needs that name listed, and `idlemax doctor` says so.
  A rebinding page can only ever present a domain name, which is why IP addresses need no listing.

### Jobs

- Jobs run without a shell, as the agent's user, with `IDLEMAX_JOB_ID` and `IDLEMAX_JOB_TAG` (which the runner uses to find a job's processes) in their environment but never the API token.
  Loader environment variables starting with `LD_` or `DYLD_` are refused.
  A job starts from a minimal environment, not the agent's: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TZ`, `TMPDIR`, `IDLEMAX_HOME`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, the XDG base directories and the user-manager address `systemd-run --user` needs.
  API keys, tokens, proxy settings and `SSH_AUTH_SOCK` in the agent's environment do not reach jobs; a job that needs one says so in the `env` of its spec.
- Every field of a job spec is bounded (name, argument and environment sizes, stdin, working directory, lists), and a spec over a limit is a 400 that names the limit.
- The store is bounded too: events keep their newest 100000 and are cut to 500 characters, at most 256 devices, 10000 unfinished jobs and 1024 pause flags are accepted (409 beyond that), device ids and names must be plain and short, and silent devices and old finished jobs are pruned.
- The agent checks its own allowlist whatever the coordinator says, matches `argv[0]` exactly, and resolves a bare name on its own `PATH` before the job's `PATH` exists.
- A job's `stdin` is written by a separate thread, so a program that never reads it cannot stop the agent's guard, kill switch or heartbeat.
- AI CLIs run only on devices that list the account, using the login already present on that device.
  idlemax never reads or copies AI credentials across devices.

### Files

- New state directories are mode 0700, and job logs, inbox notes, metrics, the database and the token are 0600, because job output can hold secrets.
  A directory that already exists keeps its mode; `idlemax doctor` warns when the state directory is open to group or others.
- `idlemax hooks`, `statusline` and `service` edit your agent configuration only when you run them.
  They change only their own entry, keep a backup with the same mode as the file it copies, create temporary files exclusively and never write through a link planted at the temporary name, and refuse a file they cannot understand.
- Values written into those files (paths with spaces, quotes, `$`, backticks) are quoted for the target: shell words in hooks, JSON strings in the OpenCode plugin, escaped text in plists and systemd units.
  Control characters are refused in plist and unit values.

### Output

- Text output removes terminal control characters from names, ids, messages and advice, so a hostile device id, job name or PR title cannot run an escape sequence in your terminal.
  Job logs and inbox notes are printed as written, like `cat`, and the TUI drops control characters itself.
- The web dashboard loads nothing remote, sends a strict Content-Security-Policy, writes every API string as text rather than HTML, and keeps the token in the browser's local storage.
  Its one script is a separate file, `/dashboard.js`, and the policy is `script-src 'self'`: an inline script, `eval` or a script from another origin does not run.
  Styles stay inline because the page sets a few `style` attributes; style cannot run code, and the policy lets the page load or send nothing outside this origin.

### Advisor

- Text from `gh` and from agent sessions is data: it goes to the AI pass as JSON the prompt calls data.
- The AI pass runs `claude -p` with no tools, no hooks and no MCP servers, takes its prompt on stdin, and its output is checked against a strict schema.
  A suggested job can only name a template you wrote, so a prompt-injected title can at most pick one of your templates, and only `advisor.auto_accept` templates queue without you.
- Quota probes only read the CLI usage metadata and session files.
