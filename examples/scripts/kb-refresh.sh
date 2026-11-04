#!/bin/sh
# idlemax job: fast-forward a notes or knowledge-base git repo and report what changed.
# Usage: KB_DIR=<repo-dir> kb-refresh.sh
# Never merges, rebases or pushes: a diverged repo is reported and left alone.
set -u
: "${KB_DIR:?set KB_DIR to the knowledge-base repo directory}"
cd "$KB_DIR" || exit 2

echo "# Knowledge-base refresh: $(basename "$KB_DIR")"
echo
before=$(git rev-parse HEAD) || exit 2
if ! out=$(git pull --ff-only --quiet 2>&1); then
  echo "Fast-forward failed; the repo is unchanged."
  echo
  echo '```'
  printf '%s\n' "$out" | tail -n 20
  echo '```'
  exit 1
fi
after=$(git rev-parse HEAD)

if [ "$before" = "$after" ]; then
  echo "Already up to date at \`$(git log -1 --format='%h %s')\`."
else
  echo "Updated \`$(git rev-parse --short "$before")\` to \`$(git rev-parse --short "$after")\`:"
  echo
  git log --format='- %h %s' "$before..$after"
  echo
  echo '```'
  git diff --stat "$before" "$after" | tail -n 20
  echo '```'
fi
