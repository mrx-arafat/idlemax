#!/bin/sh
# idlemax job: nightly health report for one repo (lint, tests, dependency audit).
# Usage: repo-health.sh <repo-dir>
# Never changes git state or installs dependencies. Failing checks are findings in the
# report, not a job failure, so the job exits 0 whenever the report was written.
set -u
repo="${1:?usage: repo-health.sh <repo-dir>}"
cd "$repo" || exit 2

failed=0

# step <title> <argv...>: runs one check and prints its result with the output tail.
step() {
  title=$1
  shift
  start=$(date +%s)
  if out=$("$@" 2>&1); then
    result="pass"
  else
    result="FAIL (exit $?)"
    failed=$((failed + 1))
  fi
  echo "## $title: $result"
  echo
  echo "\`$*\` took $(($(date +%s) - start)) s"
  echo
  echo '```'
  printf '%s\n' "$out" | tail -n 40
  echo '```'
  echo
}

skip() {
  echo "## $1: skipped"
  echo
  echo "$2"
  echo
}

echo "# Repo health: $(basename "$repo")"
echo
echo "- Path: \`$repo\`"
echo "- Branch: \`$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)\`"
echo "- Commit: \`$(git log -1 --format='%h %s' 2>/dev/null || echo unknown)\`"
echo "- Started: $(date '+%Y-%m-%d %H:%M %Z')"
echo

if [ -f Cargo.toml ]; then
  step "cargo clippy" cargo clippy --all-targets --quiet -- -D warnings
  step "cargo test" cargo test --quiet
fi

if [ -f package.json ]; then
  if [ -d node_modules ]; then
    step "npm test" npm test --silent
  else
    skip "npm test" "node_modules is missing; install dependencies by hand first."
  fi
  step "npm audit" npm audit --omit=dev --audit-level=high
fi

echo "**$failed check(s) failed.**"
exit 0
