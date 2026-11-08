## What and why

<!-- Describe what changes this pull request introduces and why they are needed. -->

## How tested

<!-- Describe how you tested these changes (focused unit tests, e2e test, or live runs in an isolated temp home). -->

## Gates checklist

- [ ] `cargo fmt --all --check` passed
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passed
- [ ] Focused tests for touched code passed (e.g. `cargo test -p idlemax-core <module>`)
- [ ] Integration tests passed if applicable (`cargo test -p idlemax --test e2e`, `cargo test -p idlemax --test examples`)
- [ ] Follows the isolated-home rule: manual runs used a temporary `$IDLEMAX_HOME`, never `~/.idlemax`
- [ ] Conventional Commit format used with subject under 72 characters
- [ ] Documentation updated if behavior or configuration changed (one sentence per line, plain dashes only)
