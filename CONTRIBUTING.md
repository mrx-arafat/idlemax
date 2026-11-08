# Contributing to idlemax

Thanks for helping.
This file covers the development loop, the gates a change must pass, and where the architecture rules live.

## Before you start

- For a bug, use the bug report form.
- For a feature, use the feature request form.
  If you plan a large change, open the request first so the approach can be discussed before you write the code.
- For a security problem, do not open a public issue.
  Follow [SECURITY.md](SECURITY.md).

## Development setup

You need a Rust toolchain on macOS or Linux.
The README records Rust 1.95, and CI builds with the stable channel.

```sh
git clone https://github.com/mrx-arafat/idlemax.git
cd idlemax
cargo build
```

The binary is `target/debug/idlemax`.
Run it without installing with `cargo run -p idlemax -- <command>`.
If you want `idlemax` on your `PATH` for manual testing, install your checkout with `cargo install --path crates/idlemax`.

## Never use your real state

idlemax keeps its state in `$IDLEMAX_HOME`, which defaults to `~/.idlemax`.
That directory holds your real token, database and job history, so a development run must never read or write it.
Start every manual run in a throwaway state dir and home:

```sh
T=$(mktemp -d)
export IDLEMAX_HOME="${T:?}/state" HOME="${T:?}/home"
mkdir -p "$IDLEMAX_HOME" "$HOME"
idlemax init
```

An `IDLEMAX_HOME` that is set but empty is an error by design, so always export a non-empty value.
The e2e test uses the same pattern and refuses to start against your real state dir.
Remove the temp directory when you are done.

## Gates

Every change must pass these before review:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p idlemax-core <module>   # focused tests for the module you changed
```

Run the focused tests for what you touched rather than the whole suite on every save.
CI runs the full workspace tests on macOS and Ubuntu, so a change that breaks another crate still fails there.

Two integration tests cover the seams:

```sh
cargo test -p idlemax --test e2e
cargo test -p idlemax --test examples
```

The first runs `init`, the coordinator, the agent and one job in a temp `IDLEMAX_HOME`.
The second keeps the example configs and job specs valid.
Run them when your change touches either area.

## Architecture rules

The layer table in the README's [Architecture](README.md#architecture) section is the rule.

- Decisions are pure functions in `crates/idlemax-core`: types, protocol, config, guard, quota, scheduler, control, reconcile, job-run and advice decisions, with time passed in as a parameter.
- Adapters in `crates/idlemax/src/{agent,coord,advisor}` and `state.rs` do the I/O: OS metrics, the process runner, SQLite, HTTP, and the `gh` and session readers.
- Delivery in `cli.rs`, `tui/`, `coord/web.rs` and `main.rs` parses arguments, makes one use-case call and renders.
- Dependencies point inward, both crates are `#![forbid(unsafe_code)]`, errors are typed with `thiserror`, and `anyhow` appears only in `main.rs` and the CLI.

Put a unit test beside each new decision and prefer small files and functions.
A behaviour change needs a test that fails before it and passes after it.

## Commits and pull requests

Use Conventional Commits: `feat(scope): ...`, `fix(scope): ...`, `docs: ...`, `test: ...`, `ci: ...`.
Keep one logical change per commit and the subject under 72 characters.
Open pull requests against `main` and fill in the template, including how you tested the change.

## CI and releases

CI (`.github/workflows/ci.yml`) runs on every push to `main` and every pull request that targets it, except when only Markdown files, `docs/` or `LICENSE` change (the generated skill under `skills/` still runs it).
A newer push cancels the run it supersedes, so push finished work in batches rather than one commit at a time:

- `cargo fmt --all --check`;
- `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` on macOS and Ubuntu;
- on Linux, shellcheck for `scripts/install.sh` and `scripts/test-install.sh`, plus a test run of the installer on macOS and Ubuntu.

A release is a `v*` tag.
`.github/workflows/release.yml` checks that the tag matches the version in `Cargo.toml`, builds the four targets, publishes the GitHub release and runs `scripts/install.sh` against it.
The full procedure is in [docs/RELEASING.md](docs/RELEASING.md).

## License

idlemax is MIT licensed; see [LICENSE](LICENSE).
