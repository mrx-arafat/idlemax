#!/bin/sh
# Installs the idlemax binary from a GitHub release. POSIX sh, never needs sudo.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/mrx-arafat/idlemax/main/scripts/install.sh | sh
#        sh install.sh [--dry-run]
#
#   --dry-run             print what would be downloaded and installed, change nothing
#
# Environment:
#   IDLEMAX_VERSION       release to install, for example 0.2.0 (default: the latest release)
#   IDLEMAX_INSTALL_DIR   absolute directory for the binary (default: ~/.local/bin)
#
# Supported: macOS and Linux (glibc), on arm64 and x86_64. The release archive is checked
# against its published sha256 before anything is installed, and the new binary must run
# (`idlemax --version`) before it replaces an existing one. Any failure stops with a message
# and leaves the install directory as it was.
set -eu

REPO="mrx-arafat/idlemax"
BASE="https://github.com/${REPO}"

TMP=""
STAGED=""

die() {
  printf 'idlemax install: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  [ -z "$STAGED" ] || rm -f "$STAGED"
  [ -z "$TMP" ] || rm -rf "$TMP"
}

usage() {
  cat <<'EOF_USAGE'
Usage: install.sh [--dry-run]

  --dry-run             print what would be downloaded and installed, change nothing

Environment:
  IDLEMAX_VERSION       release to install, for example 0.2.0 (default: the latest release)
  IDLEMAX_INSTALL_DIR   absolute directory for the binary (default: ~/.local/bin)
EOF_USAGE
}

# Prints the release target triple for this machine, or returns 1.
detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      # A shell under Rosetta reports x86_64 on Apple Silicon; install the native build.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
        arch=arm64
      fi
      case "$arch" in
        arm64 | aarch64) echo aarch64-apple-darwin ;;
        x86_64) echo x86_64-apple-darwin ;;
        *) return 1 ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64 | amd64) echo x86_64-unknown-linux-gnu ;;
        aarch64 | arm64) echo aarch64-unknown-linux-gnu ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

# True for a plain release number such as 0.2.0 or 0.2.0-rc.1.
valid_version() {
  printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'
}

fetch() {
  curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --connect-timeout 20 -o "$2" "$1"
}

# GitHub redirects /releases/latest to /releases/tag/vX.Y.Z; reading the redirect
# avoids the rate-limited API.
latest_version() {
  final=$(curl -fsSL --proto '=https' --tlsv1.2 --retry 3 --connect-timeout 20 \
    -o /dev/null -w '%{url_effective}' "${BASE}/releases/latest") || return 1
  tag=${final##*/}
  case "$tag" in
    v*) printf '%s\n' "${tag#v}" ;;
    *) printf '\n' ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= *//'
  else
    return 1
  fi
}

# Compares the archive's sha256 with the first field of its .sha256 file.
verify_checksum() {
  archive=$1
  sumfile=$2
  read -r expected _ <"$sumfile" || true
  expected=$(printf '%s' "${expected:-}" | tr 'A-F' 'a-f')
  if [ "${#expected}" -ne 64 ] || [ -n "$(printf '%s' "$expected" | tr -d '0-9a-f')" ]; then
    die "the published checksum file is malformed; nothing was installed"
  fi
  actual=$(sha256_of "$archive") ||
    die "no sha256 tool found (need sha256sum, shasum or openssl); nothing was installed"
  if [ "$actual" != "$expected" ]; then
    die "checksum mismatch for $(basename "$archive"): expected $expected, got $actual; nothing was installed"
  fi
}

main() {
  dry_run=0
  for arg in "$@"; do
    case "$arg" in
      --dry-run) dry_run=1 ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "unknown option: $arg (try --help)" ;;
    esac
  done

  target=$(detect_target) ||
    die "unsupported platform $(uname -s) $(uname -m); releases cover macOS and Linux on arm64 and x86_64. Build from source: git clone ${BASE}.git && cd idlemax && cargo install --path crates/idlemax"

  if [ -n "${IDLEMAX_INSTALL_DIR+set}" ] && [ -z "$IDLEMAX_INSTALL_DIR" ]; then
    die "IDLEMAX_INSTALL_DIR is set but empty; unset it or give an absolute directory"
  fi
  dir=${IDLEMAX_INSTALL_DIR:-}
  if [ -z "$dir" ]; then
    [ -n "${HOME:-}" ] || die "HOME is not set; set IDLEMAX_INSTALL_DIR to an absolute directory"
    dir="$HOME/.local/bin"
  fi
  case "$dir" in
    /*) ;;
    *) die "IDLEMAX_INSTALL_DIR must be an absolute path, got: $dir" ;;
  esac

  command -v curl >/dev/null 2>&1 || die "curl is required; install it with your package manager and run this again"
  command -v tar >/dev/null 2>&1 || die "tar is required; install it with your package manager and run this again"

  version=${IDLEMAX_VERSION:-}
  version=${version#v}
  if [ -z "$version" ]; then
    version=$(latest_version) ||
      die "cannot look up the latest release (network down?); set IDLEMAX_VERSION, for example IDLEMAX_VERSION=0.2.0"
    [ -n "$version" ] ||
      die "no release has been published yet (${BASE}/releases). Build from source: git clone ${BASE}.git && cd idlemax && cargo install --path crates/idlemax"
  fi
  valid_version "$version" ||
    die "no usable release version (got '$version'); see ${BASE}/releases or set IDLEMAX_VERSION, for example IDLEMAX_VERSION=0.2.0"

  name="idlemax-${version}-${target}"
  url="${BASE}/releases/download/v${version}/${name}.tar.gz"

  if [ "$dry_run" -eq 1 ]; then
    printf 'idlemax install (dry run, nothing changed)\n'
    printf '  target:  %s\n  version: %s\n  asset:   %s\n  into:    %s/idlemax\n' \
      "$target" "$version" "$url" "$dir"
    exit 0
  fi

  trap cleanup EXIT
  trap 'exit 1' INT TERM HUP
  TMP=$(mktemp -d "${TMPDIR:-/tmp}/idlemax-install.XXXXXX") || die "cannot create a temporary directory"

  printf 'Downloading idlemax %s for %s\n' "$version" "$target"
  fetch "$url" "$TMP/$name.tar.gz" ||
    die "download failed: $url (does release v${version} exist for $target?)"
  fetch "$url.sha256" "$TMP/$name.tar.gz.sha256" ||
    die "download failed: $url.sha256; refusing to install without a checksum"
  verify_checksum "$TMP/$name.tar.gz" "$TMP/$name.tar.gz.sha256"

  mkdir "$TMP/extract"
  tar -xzf "$TMP/$name.tar.gz" -C "$TMP/extract" || die "cannot unpack the archive; nothing was installed"
  bin="$TMP/extract/$name/idlemax"
  [ -f "$bin" ] || die "the archive does not contain $name/idlemax; nothing was installed"
  chmod 755 "$bin"
  "$bin" --version >/dev/null 2>&1 ||
    die "the downloaded binary does not run on this system. Linux builds need a recent glibc (Ubuntu 24.04 level) and do not support musl such as Alpine. Build from source: git clone ${BASE}.git && cd idlemax && cargo install --path crates/idlemax"

  mkdir -p "$dir" || die "cannot create $dir; set IDLEMAX_INSTALL_DIR to a directory you own"
  STAGED="$dir/.idlemax.$$"
  cp "$bin" "$STAGED" || die "cannot write to $dir; set IDLEMAX_INSTALL_DIR to a directory you own (no sudo is used)"
  chmod 755 "$STAGED"
  mv -f "$STAGED" "$dir/idlemax" || die "cannot replace $dir/idlemax"
  STAGED=""

  printf 'Installed %s\n' "$("$dir/idlemax" --version)"
  printf '  %s/idlemax\n' "$dir"
  case ":${PATH:-}:" in
    *":$dir:"*) ;;
    *)
      printf '\n%s is not on your PATH. Add this line to your shell profile:\n' "$dir"
      printf '  export PATH="%s:%s"\n' "$dir" "\$PATH"
      ;;
  esac
  printf '\nNext: idlemax setup, then idlemax --help\n'
}

main "$@"
