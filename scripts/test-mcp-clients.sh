#!/bin/sh
# Adds `idlemax mcp` to each installed agent CLI in a throwaway home and checks that the CLI
# lists it. It sends no prompt, so it spends no quota and starts no session. A CLI that is not
# installed is skipped. Usage: sh scripts/test-mcp-clients.sh [path/to/idlemax]
set -eu

IDLEMAX=${1:-idlemax}
REAL_HOME=$HOME
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT INT TERM
export HOME="$T/home" IDLEMAX_HOME="$T/state"
mkdir -p "$HOME" "$IDLEMAX_HOME"
unset CLAUDE_CONFIG_DIR CODEX_HOME OPENCODE_CONFIG_DIR XDG_CONFIG_HOME
test -n "$IDLEMAX_HOME" && test "$HOME" != "$REAL_HOME" && test -d "$IDLEMAX_HOME" || exit 1

failed=0

# check <cli> <add command...>: runs the add line, then `<cli> mcp list`.
check() {
    cli=$1
    shift
    if ! command -v "$cli" >/dev/null 2>&1; then
        echo "skip: $cli is not installed"
        return 0
    fi
    test "$HOME" != "$REAL_HOME" || exit 1
    touch "$T/before"
    sleep 1
    if ! "$@" >"$T/$cli.add" 2>&1; then
        echo "FAIL: $cli: the add line failed"
        cat "$T/$cli.add"
        failed=1
        return 0
    fi
    if [ -z "$(find "$T" -type f -newer "$T/before" ! -name "$cli.add" | head -n 1)" ]; then
        echo "FAIL: $cli changed no file under the temp home: it wrote its settings elsewhere"
        failed=1
        return 0
    fi
    if "$cli" mcp list 2>&1 | grep -q idlemax; then
        echo "ok: $cli lists idlemax"
    else
        echo "FAIL: $cli mcp list does not show idlemax"
        failed=1
    fi
}

check claude claude mcp add --scope user idlemax -- "$IDLEMAX" mcp
check codex codex mcp add idlemax -- "$IDLEMAX" mcp
check opencode opencode mcp add --global idlemax -- "$IDLEMAX" mcp
check agy agy mcp add idlemax "$IDLEMAX" mcp

exit "$failed"
