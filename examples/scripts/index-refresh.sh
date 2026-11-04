#!/bin/sh
# idlemax job: incremental CodeGraph sync for every repo in $REPOS (space separated).
# Prints a markdown summary for the inbox; exits 1 if any sync failed so the job retries.
set -u
: "${REPOS:?set REPOS to a space separated list of repo directories}"

echo "# Index refresh"
echo
rc=0
for repo in $REPOS; do
  if [ ! -d "$repo/.codegraph" ]; then
    echo "- \`$repo\`: skipped, no .codegraph index (run \`codegraph init\` there once by hand)"
    continue
  fi
  start=$(date +%s)
  if out=$(codegraph sync -q "$repo" 2>&1); then
    echo "- \`$repo\`: synced in $(($(date +%s) - start)) s"
  else
    rc=1
    echo "- \`$repo\`: FAILED"
    echo
    echo '```'
    printf '%s\n' "$out" | tail -n 20
    echo '```'
  fi
done
exit "$rc"
