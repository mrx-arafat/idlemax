#!/bin/sh
# Tests the maintainer scripts offline in temp dirs: scripts/changelog.sh orders a prerelease
# tag before its release, and scripts/verify-linux.sh --self-test runs to the end on the
# macOS system bash (3.2) and removes its temp dir. Usage: sh scripts/test-scripts.sh
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d) || exit 1
trap 'rm -rf "${T:?}"' EXIT
failed=0
fail() {
  printf 'FAIL: %s\n' "$*" >&2
  failed=1
}

# A repository whose newest tag is a release that comes after its own release candidate.
repo="$T/repo"
mkdir -p "$repo" "$T/tmp"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
git -C "$repo" config commit.gpgsign false
commit() { git -C "$repo" commit -q --allow-empty -m "$1"; }
commit "feat: first"
git -C "$repo" tag v0.1.0
commit "fix: candidate fix"
git -C "$repo" tag v0.2.0-rc.1
commit "feat: final feature"
git -C "$repo" tag v0.2.0
commit "fix: after the release"

(cd "$repo" && bash "$HERE/changelog.sh" "$T/changelog.md" >/dev/null 2>&1) || fail "changelog.sh failed"
unreleased=$(sed -n '/^## \[Unreleased\]/,/^## \[0/p' "$T/changelog.md")
case $unreleased in
  *"After the release"*) ;;
  *) fail "Unreleased lacks the commit made after v0.2.0" ;;
esac
case $unreleased in
  *"Final feature"*) fail "Unreleased holds a commit that v0.2.0 already released" ;;
esac
order=$(grep -n '^## \[0' "$T/changelog.md" | sed 's/^[0-9]*:## \[\([^]]*\)\].*/\1/' | tr '\n' ' ')
[ "$order" = "0.2.0 0.2.0-rc.1 0.1.0 " ] || fail "release sections are in the order: $order"

# The self-test must print its summary and leave nothing behind, even on bash 3.2.
shell=bash
[ -x /bin/bash ] && shell=/bin/bash
out=$(TMPDIR="$T/tmp" "$shell" "$HERE/verify-linux.sh" --self-test 2>&1)
status=$?
[ "$status" -eq 0 ] || fail "verify-linux.sh --self-test exited $status: $out"
case $out in
  *"PASS: the parser fixtures matched every expected value."*) ;;
  *) fail "verify-linux.sh --self-test printed no PASS summary: $out" ;;
esac
[ -z "$(ls -A "$T/tmp")" ] || fail "verify-linux.sh --self-test left $(ls -A "$T/tmp")"

[ "$failed" -eq 0 ] && echo "scripts: ok"
exit "$failed"
