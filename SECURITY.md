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
- Job execution: process isolation, environment variable filtering (refusing `LD_*` and `DYLD_*` variables and withholding the API token), argv execution without shell interpolation, allowlist enforcement, and OS resource limits.
- Statusline wrapper: safe capture of usage metrics in `packaging/claude-statusline-capture.sh`, atomic file replacement, avoiding credential exposure, and non-blocking failure semantics.

Third-party dependencies with known vulnerabilities that do not affect idlemax's execution paths are out of scope unless accompanied by an exploitable vector.

## Security model summary

idlemax is built around several core security guarantees:

- One bearer token (32 random bytes as hex, file mode 0600) guards every API call except `/healthz` and `/`, and is compared in constant time.
- Anyone holding the token can run any allowlisted program with any arguments on your devices.
  Treat the token like an SSH key and keep allowlists narrow, because allowlisting generic interpreters such as `sh` or `python3` allows arbitrary code execution.
- The coordinator refuses to bind `0.0.0.0` or `[::]`.
  Its traffic is plain HTTP, so bind it to a private network such as a tailnet that encrypts transit traffic.
- The local agent API listens on loopback only.
- Jobs run without a shell, as the agent's user, with `IDLEMAX_JOB_ID` in their environment but never the API token.
  Loader environment variables starting with `LD_` or `DYLD_` are refused.
- AI CLIs run only on devices that list the account, using the login already present on that device.
  idlemax never reads or copies AI credentials across devices.
- The web dashboard loads nothing remote, sends a strict Content-Security-Policy, writes every API string as text rather than HTML, and keeps the token in the browser's local storage.
- Quota probes only read the CLI usage metadata and session files.
