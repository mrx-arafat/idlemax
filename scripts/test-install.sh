#!/bin/sh
# Tests scripts/install.sh offline: fake curl, uname and sysctl on PATH, a fake release in a
# temp dir, and every install goes to a temp IDLEMAX_INSTALL_DIR. Nothing touches the network
# or the real home. Usage: sh scripts/test-install.sh
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
INSTALL="$HERE/install.sh"
T=$(mktemp -d) || exit 1
trap 'rm -rf "${T:?}"' EXIT
mkdir -p "$T/shim" "$T/releases" "$T/home" "$T/tmp" "$T/pack"

# A throwaway environment for every run of the installer.
export HOME="$T/home" TMPDIR="$T/tmp" FAKE_RELEASES="$T/releases"
export FAKE_LATEST_URL="https://github.com/mrx-arafat/idlemax/releases/tag/v0.9.0"
export COPYFILE_DISABLE=1
PATH="$T/shim:$PATH"
unset IDLEMAX_VERSION IDLEMAX_INSTALL_DIR

cat >"$T/shim/uname" <<'EOF'
#!/bin/sh
case "$1" in -m) echo "$FAKE_ARCH" ;; *) echo "$FAKE_OS" ;; esac
EOF
cat >"$T/shim/sysctl" <<'EOF'
#!/bin/sh
echo "${FAKE_ARM64:-0}"
EOF
cat >"$T/shim/curl" <<'EOF'
#!/bin/sh
out=""; fmt=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w) fmt=$2; shift 2 ;;
    --proto | --retry | --connect-timeout) shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
case "$url" in
  */releases/latest) [ -z "$fmt" ] || printf '%s' "$FAKE_LATEST_URL" ;;
  */releases/download/*)
    file="$FAKE_RELEASES/${url##*/}"
    [ -f "$file" ] || exit 22
    [ "$out" = /dev/null ] || cp "$file" "$out"
    ;;
  *) exit 6 ;;
esac
EOF
chmod 755 "$T/shim/uname" "$T/shim/sysctl" "$T/shim/curl"

fails=0
# check NAME COMMAND [ARGS...]: passes when the command succeeds.
check() {
  name=$1
  shift
  if "$@"; then
    printf 'ok   %s\n' "$name"
  else
    printf 'FAIL %s\n' "$name"
    fails=$((fails + 1))
  fi
}

# inst VAR=value ...   runs the installer with those extra variables; dry adds --dry-run.
# Output goes to $T/out and the installer's status to RC.
inst() {
  env "$@" sh "$INSTALL" >"$T/out" 2>&1
  RC=$?
}
dry() {
  env "$@" sh "$INSTALL" --dry-run >"$T/out" 2>&1
  RC=$?
}

ok() { [ "$RC" -eq 0 ]; }
said() { grep -q "$1" "$T/out"; }
refused() { [ "$RC" -ne 0 ] && said "$1"; }
refused_keeping_old() { refused "$1" && [ "$(cat "$BIN/idlemax")" = old ]; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

# make_asset TARGET BODY: a release archive whose idlemax is the script BODY, plus its checksum.
make_asset() {
  name="idlemax-0.9.0-$1"
  rm -rf "${T:?}/pack" && mkdir -p "$T/pack/$name"
  printf '#!/bin/sh\n%s\n' "$2" >"$T/pack/$name/idlemax"
  chmod 755 "$T/pack/$name/idlemax"
  tar -czf "$T/releases/$name.tar.gz" -C "$T/pack" "$name"
  (cd "$T/releases" && sha256 "$name.tar.gz" >"$name.tar.gz.sha256")
}

# Platform detection, through --dry-run.
detect() { # os arch arm64flag expected-target
  dry FAKE_OS="$1" FAKE_ARCH="$2" FAKE_ARM64="$3" IDLEMAX_VERSION=0.9.0
  ok && said "target:  $4\$"
}
check "detects Apple Silicon" detect Darwin arm64 1 aarch64-apple-darwin
check "detects Intel Mac" detect Darwin x86_64 0 x86_64-apple-darwin
check "prefers the native build under Rosetta" detect Darwin x86_64 1 aarch64-apple-darwin
check "detects Linux x86_64" detect Linux x86_64 0 x86_64-unknown-linux-gnu
check "detects Linux arm64" detect Linux aarch64 0 aarch64-unknown-linux-gnu

dry FAKE_OS=Linux FAKE_ARCH=armv7l IDLEMAX_VERSION=0.9.0
check "refuses an unsupported CPU" refused "unsupported platform Linux armv7l"
dry FAKE_OS=FreeBSD FAKE_ARCH=amd64 IDLEMAX_VERSION=0.9.0
check "refuses an unsupported OS" refused "unsupported platform FreeBSD"
dry FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_VERSION=v0.9.0
check "installs to ~/.local/bin by default" said "into:    $T/home/.local/bin/idlemax\$"

# Bad input is refused before any download.
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_VERSION='1.0;touch evil'
check "rejects a malformed version" refused "no usable release version"
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_VERSION=0.9.0 IDLEMAX_INSTALL_DIR=relative/bin
check "rejects a relative install dir" refused "must be an absolute path"

# Before the first release GitHub redirects /releases/latest to the releases list.
inst FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_LATEST_URL=https://github.com/mrx-arafat/idlemax/releases
check "explains that no release exists yet" refused "no release has been published yet"

# Happy path: latest release found by redirect, checksum verified, binary installed and runnable.
BIN="$T/bin"
make_asset x86_64-unknown-linux-gnu 'echo "0.9.0"'
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_INSTALL_DIR="$BIN"
check "installs the latest release" ok
check "names what it installed" said "^Installed idlemax 0.9.0\$"
version_ok() { [ "$("$BIN/idlemax" --version)" = "0.9.0" ]; }
check "installed binary runs" version_ok
clean() { [ -z "$(ls -A "$T/tmp")" ] && [ "$(ls -A "$BIN")" = idlemax ]; }
check "leaves no temp files behind" clean

# Fail closed: an existing install survives every failure.
ASSET=idlemax-0.9.0-x86_64-unknown-linux-gnu.tar.gz
printf 'old\n' >"$BIN/idlemax"
printf '%064d  %s\n' 0 "$ASSET" >"$T/releases/$ASSET.sha256"
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_INSTALL_DIR="$BIN"
check "refuses a wrong checksum" refused_keeping_old "checksum mismatch"

rm "$T/releases/$ASSET.sha256"
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_INSTALL_DIR="$BIN"
check "refuses a missing checksum" refused_keeping_old "refusing to install without a checksum"

make_asset x86_64-unknown-linux-gnu 'exit 1'
inst FAKE_OS=Linux FAKE_ARCH=x86_64 IDLEMAX_INSTALL_DIR="$BIN"
check "refuses a binary that cannot run" refused_keeping_old "does not run on this system"

if [ "$fails" -ne 0 ]; then
  printf '%s check(s) failed\n' "$fails"
  exit 1
fi
printf 'all install checks passed\n'
