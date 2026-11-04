#!/bin/sh
# Claude Code statusline wrapper: saves the statusline JSON (it carries `rate_limits`) to
# $IDLEMAX_HOME/agent/claude-statusline.json for the idlemax quota probe, then runs your
# real statusline command with the same input. Without this file Claude accounts are never
# spendable. It never fails the statusline: capture errors are ignored.
#
# ~/.claude/settings.json:
#   "statusLine": { "type": "command",
#                   "command": "/path/to/claude-statusline-capture.sh <your statusline command>" }
set -u
input=$(cat)

# Unset means ~/.idlemax; set but empty means no capture, never a fallback.
home=${IDLEMAX_HOME-${HOME:-}/.idlemax}
dir="$home/agent"
if [ -n "${IDLEMAX_HOME-x}" ] && [ -d "$dir" ]; then
  tmp="$dir/.claude-statusline.$$"
  if printf '%s' "$input" >"$tmp" 2>/dev/null; then
    mv -f "$tmp" "$dir/claude-statusline.json" 2>/dev/null || rm -f "$tmp"
  else
    rm -f "$tmp"
  fi
fi

if [ "$#" -gt 0 ]; then
  printf '%s' "$input" | "$@"
fi
