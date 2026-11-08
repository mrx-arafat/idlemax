# AGENTS.md

Guidance for coding agents working on idlemax.
CONTRIBUTING.md is the longer human version; this file is the short one loaded every session.

## What this is

idlemax keeps a developer's Mac and server busy on useful jobs using only spare capacity, and spends leftover AI subscription quota before it resets.
It is one Rust binary, `idlemax`, with two daemons:

- `idlemax coord`: job queue, SQLite, HTTP API and web dashboard (default `127.0.0.1:7878`).
- `idlemax agent`: one per device; samples metrics, runs the guard, runs jobs under OS throttles, serves a local API on `127.0.0.1:7879`.

The CLI, TUI and dashboard talk to the coordinator API.
It must never slow the machine it runs on, so the guard and the kill switch file `$IDLEMAX_HOME/PAUSE` are safety features, not options.

## Layer map

```text
crates/idlemax-core/src/   domain, pure, no I/O, no clock: advice, config, control, doctor, explain, guard,
                           install, job_run, notify, paths, protocol, quota, reconcile, schedule,
                           scheduler, types
crates/idlemax/src/        agent/, coord/ (store/schema.rs holds the migrations), advisor/,
                           doctor/ (adapters); cli.rs + cli/, tui/, coord/web.rs, main.rs (delivery)
crates/idlemax/tests/      e2e.rs, examples.rs, doctor.rs, logs_inbox.rs and other integration tests
examples/  packaging/  scripts/   example configs and jobs, service units, install and changelog scripts
skills/                    the generated agent skill (see Traps)
docs/                      API.md, ROADMAP.md, ROLLOUT.md, RELEASING.md
```

Rules that keep it clean:

- Dependencies point inward.
  Decisions are pure functions in `idlemax-core` with unit tests, and time is a `now_ms` parameter, never a clock call.
- Adapters stay thin: they gather facts or do I/O and call the pure decision.
  Delivery parses arguments, makes one use-case call and renders.
- Files under about 400 lines and functions under about 50.
  Each module starts with a `//!` line naming its responsibility and layer.
- Typed errors with `thiserror`; `anyhow` only in `main.rs` and the CLI.
  No `unwrap`, `expect` or `panic!` outside tests and documented startup invariants.
  Both crates are `#![forbid(unsafe_code)]`.
- No abstraction without a second implementation or a test that needs it.
- Mark a deliberate shortcut with `// SHORTCUT: <ceiling>, <upgrade trigger>`.
  `grep -rnE '(#|//) ?SHORTCUT:' crates` lists them.
- User-facing commands take `--json`, and an error says what to do next.

## Gates

Every commit passes these:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Then run the focused tests for what you changed, not the whole suite (CI runs it on macOS and Ubuntu):

```sh
cargo test -p idlemax-core guard           # a core module: cargo test -p idlemax-core <module>
cargo test -p idlemax --bin idlemax doctor # unit tests inside the binary crate, by name filter
cargo test -p idlemax --test e2e           # coord + agent + one job in a temp home (about 12 s)
cargo test -p idlemax --test examples      # example configs and job specs stay valid
```

Other integration tests are `--test <file name>` under `crates/idlemax/tests/`.
A behaviour change gets a test that fails first, then passes.
Add the fewest tests that pin the behaviour.
Touching `scripts/install.sh`: run `sh scripts/test-install.sh` (offline, temp home).

On a small machine use `export CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`, prefer `cargo check -p <crate>`, and skip `--release`.
Run the binary from the checkout with `cargo run -p idlemax -- <command>` or `target/debug/idlemax`.

## Isolated home: never touch the real state

`$IDLEMAX_HOME` (default `~/.idlemax`) holds the real token, database and job history.
Every manual run, script or sub-agent uses a throwaway home, and asserts it before each invocation:

```sh
REAL_HOME=$HOME
T=$(mktemp -d) && export IDLEMAX_HOME="${T:?}/state" HOME="${T:?}/home" && mkdir -p "$IDLEMAX_HOME" "$HOME"
test -n "$IDLEMAX_HOME" && test "$IDLEMAX_HOME" != "$REAL_HOME/.idlemax" && test -d "$IDLEMAX_HOME" || exit 1
idlemax init
```

- A set but empty `IDLEMAX_HOME` is an error by design (`IDLEMAX_HOME is set but empty`), so never export it empty.
- Also set `HOME`: `idlemax service install` writes a LaunchAgent or systemd user unit under it, and `idlemax statusline install` and `idlemax hooks install` edit the settings of Claude Code, Codex and OpenCode there.
  Run those only in the throwaway home, and unset `CLAUDE_CONFIG_DIR` and `CODEX_HOME`, which would point them back at the real directories.
- Use non-default ports in a dev config (`coordinator.listen`, `agent.listen`); a real agent may already hold 7878 or 7879.
- Never configure an AI account or queue an `ai_account` job in a dev run: it would launch the real `claude -p`, `codex exec` or `agy -p` and spend quota.
- Never run `launchctl` or `systemctl` against the real user session.
- Delete the temp dir when you are done, and stop any daemon you started.

## Traps

- Store migrations are numbered and append-only: add the next `Vn` to `MIGRATIONS` in `crates/idlemax/src/coord/store/schema.rs`.
  Never edit an applied migration, and two branches that each add the next number will collide, so check `main` before you add one.
- `CHANGELOG.md` is generated from Conventional Commit subjects by `scripts/changelog.sh`.
  Never edit it by hand; fix the commit history or the script.
- `docs/PLAN.md`, `docs/status/` and `docs/contract-requests.md` are gitignored local notes.
  Stage files by name and never use `git add -f`.
- `skills/idlemax/SKILL.md` is generated from the text in `crates/idlemax/src/cli/home/render.rs`.
  After changing the description or command list there, run `idlemax skill > skills/idlemax/SKILL.md`; CI runs `idlemax skill --check`.
- Wire formats and lease or control semantics live in `docs/API.md`; change the code and that file together.
  `README.md` is the user-facing doc: update the section a behaviour change makes wrong, and nothing else.

## Commits

Conventional Commits: `feat(cli): ...`, `fix(agent): ...`, `docs: ...`, `test: ...`, `ci: ...`.
One logical change per commit, subject under 72 characters, a short body saying why.
No em dashes anywhere (use `-`), and no `Co-Authored-By` or "Generated with" trailer.
Pull requests target `main`.
