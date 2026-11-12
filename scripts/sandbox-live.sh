#!/bin/sh
# Runs the worker sandbox's live tests (`live_` in `agent/worker/sandbox/live_tests.rs`) against
# a real bubblewrap, in a throwaway Docker container with no network.
#
# Why a container: a host whose AppArmor restricts unprivileged user namespaces (Ubuntu 24.04)
# cannot run an unconfined bwrap. The container is privileged and runs a setuid-root copy of
# the Ubuntu bwrap binary as the developer's uid, which stands in for the AppArmor profile; the
# namespaces and mounts are the same. The host's root is mounted read-only; only a scratch
# folder is writable, as /srv inside, and it holds the fake owner's home.
#
# Usage: scripts/sandbox-live.sh [test name filter]
# Needs: docker, the alpine image, and a bwrap binary: BWRAP=/path/to/bwrap (extract one with
# `apt-get download bubblewrap && dpkg-deb -x bubblewrap_*.deb x`).
set -eu

filter=${1:-live_}
: "${BWRAP:?set BWRAP to a bwrap binary}"
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

cargo build -q -p idlemax
test_bin=$(cargo test -q -p idlemax --bin idlemax --no-run --message-format=json 2>/dev/null |
    sed -n 's/.*"executable":"\([^"]*idlemax-[^"]*\)".*/\1/p' | tail -n 1)
test -x "$test_bin" || { echo "no test binary found" >&2; exit 1; }

scratch=$(mktemp -d)
trap 'docker run --rm -v "$scratch:/s" alpine rm -rf /s/home /s/bwrap >/dev/null 2>&1; rm -rf "$scratch"' EXIT
cp "$BWRAP" "$scratch/bwrap.src"
uid=$(id -u)
gid=$(id -g)

docker run --rm --privileged --network none \
    -v /:/host:ro -v "$scratch:/host/srv" alpine sh -c "
    set -e
    mount --rbind /proc /host/proc
    mount --rbind /dev /host/dev
    chroot /host /bin/sh -c '
        set -e
        cp /srv/bwrap.src /srv/bwrap && chown root:root /srv/bwrap && chmod 4755 /srv/bwrap
        mkdir -p /srv/home && chown $uid:$gid /srv/home
        cd /srv
        exec setpriv --reuid=$uid --regid=$gid --clear-groups env -i \
            HOME=/srv/home PATH=/usr/bin:/bin \
            IDLEMAX_LIVE_BWRAP=/srv/bwrap IDLEMAX_LIVE_BIN=$root/target/debug/idlemax \
            $test_bin --ignored --test-threads=1 $filter
    '
"
