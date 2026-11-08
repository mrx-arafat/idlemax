# Releasing idlemax

A release is a `v*` tag.
Pushing the tag runs `.github/workflows/release.yml`, which builds the binaries, publishes the GitHub release and then installs it again with `scripts/install.sh` as a check.
Nothing else is needed to ship.

## Steps

1. Pick the tag on a commit whose CI run on `main` is green.
2. Set `version` under `[workspace.package]` in `Cargo.toml`.
   Also set the `idlemax-core` version in `crates/idlemax/Cargo.toml` to the same number, or Cargo cannot resolve the path dependency.
3. Run `cargo check` so `Cargo.lock` follows, because CI builds with `--locked`.
4. Run `scripts/changelog.sh --tag vX.Y.Z` to regenerate `CHANGELOG.md`.
   Never edit that file by hand.
5. Commit the version bump, `Cargo.lock` and `CHANGELOG.md`, and wait for CI on `main`.
6. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

## What the workflow does

| Job | What it does |
|---|---|
| preflight | Fails unless the tag is `vMAJOR.MINOR.PATCH` (optionally `-suffix`) and equals the version in `Cargo.toml` |
| build | Builds `idlemax` for four targets, packs `idlemax-<version>-<target>.tar.gz` with a `.sha256`, unpacks it and runs `idlemax --version` |
| publish | Attaches all archives to one release, only after every build passed. A tag with a `-suffix` becomes a pre-release |
| install-check | Runs `scripts/install.sh` against the published release on Linux x86_64, Linux arm64 and macOS arm64 |

Targets are `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`.
Each archive holds the `idlemax` binary, `LICENSE`, `README.md`, `CHANGELOG.md` and the `packaging/` service files.

## When a run fails

- A failed build or check job: fix the cause on `main`, delete the tag and push it again on the new commit.
  Nothing is published until all four builds pass, so there is no partial release to clean up.
- A flaky job: use "Re-run failed jobs" on the run.
- A failed install check after publishing: the release is already public.
  Fix `scripts/install.sh` on `main`, since users fetch it from there, and tag a patch release only if the binaries are at fault.

## Limits

- Linux binaries are built on the `ubuntu-latest` and `ubuntu-24.04-arm` images, so they need a glibc as new as that image has.
  Older distributions and musl systems such as Alpine must build from source.
- The `.sha256` file is published next to the archive, so it catches a corrupt or truncated download, not a compromised release.
  Archives are not signed yet.
