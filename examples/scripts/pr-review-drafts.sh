#!/bin/sh
# idlemax job: draft review notes for open PRs that wait on your review.
# Usage: pr-review-drafts.sh claude|codex   (MAX_PRS caps the batch, default 3)
# Read-only by construction: gh fetches the PR text, the AI gets it on stdin with no tools
# and no MCP servers, and nothing is posted anywhere. PRs run one after another, never in
# parallel, and the script stops at the first AI error (a usage limit included) without retrying.
set -eu
provider="${1:?usage: pr-review-drafts.sh claude|codex}"
max="${MAX_PRS:-3}"

prompt='Draft a code review of the pull request on stdin (metadata, then the diff).
Write markdown with: a 3 line summary; the riskiest changes; concrete bugs or questions, each with file and line;
a suggested verdict (approve, comment or request changes) with one line of reasoning.
Only discuss code that is in the diff. Do not post anything.'

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT INT TERM

urls=$(gh search prs --review-requested=@me --state=open --limit "$max" --json url --jq '.[].url')

echo "# PR review drafts ($provider)"
echo
echo "Drafts only, nothing was posted. Generated $(date '+%Y-%m-%d %H:%M %Z')."
if [ -z "$urls" ]; then
  echo
  echo "No open PRs are waiting on your review."
  exit 0
fi

for url in $urls; do
  echo
  echo "## $url"
  echo
  {
    gh pr view "$url" --json title,author,baseRefName,headRefName,body \
      --jq '"Title: \(.title)\nAuthor: \(.author.login)\nBranch: \(.headRefName) -> \(.baseRefName)\n\n\(.body)"'
    echo
    gh pr diff "$url" | head -c 150000
  } >"$tmp"
  case "$provider" in
    # --no-session-persistence keeps this job from looking like interactive use of claude.
    claude) claude -p "$prompt" --permission-mode dontAsk --strict-mcp-config \
      --no-session-persistence --output-format text <"$tmp" ;;
    codex) codex exec --sandbox read-only --skip-git-repo-check --ephemeral "$prompt" <"$tmp" ;;
    *)
      echo "unknown provider: $provider" >&2
      exit 2
      ;;
  esac
done
