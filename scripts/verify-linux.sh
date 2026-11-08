#!/usr/bin/env bash
# Verify the Linux agent and job caps on a real systemd host.
#
# Run as the normal user that will own the agent (never root), on the Linux
# server, with a built idlemax binary:
#
#   IDLEMAX_BIN=/usr/local/bin/idlemax ./scripts/verify-linux.sh
#
# The script creates a temporary IDLEMAX_HOME and HOME, starts a loopback
# coordinator and an agent in capped `systemd-run --user --scope` units, compares
# every sampled signal with /proc and /sys, runs one memory-hog job and one
# CPU-bound job through idlemax, verifies both shipped units with
# `systemd-analyze verify`, prints a PASS/FAIL table and removes everything.
#
# On macOS only the parser self-test runs:
#
#   ./scripts/verify-linux.sh --self-test
#
# Environment:
#   IDLEMAX_BIN          idlemax binary to test (required in a real run)
#   IDLEMAX_UNITS_DIR    directory with the two .service files
#                        (default: ../packaging/linux next to this script)
#   IDLEMAX_VERIFY_KEEP  set to 1 to keep the temp dir and its logs on failure
#
# The script itself needs systemd 254 or newer because it passes
# --expand-environment=no to systemd-run. It fails closed if any prerequisite,
# signal comparison, cap check or unit check does not pass.

set -Eeuo pipefail
IFS=$'\n\t'
umask 077
export LC_ALL=C
export LANG=C

SCRIPT_PATH=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SCRIPT_PATH="$SCRIPT_PATH/$(basename -- "${BASH_SOURCE[0]}")"
REPO_DIR=$(cd -- "$(dirname -- "$SCRIPT_PATH")/.." && pwd)

CHECK_NAMES=()
CHECK_RESULTS=()
CHECK_DETAILS=()
FAILED=0
SELFTEST=0

TMP=""
HELPER=""
STATE=""
FAKE_HOME=""
COORD_UNIT=""
AGENT_UNIT=""
COORD_PID=""
AGENT_PID=""
JOB_UNITS=()
JOB_STATUS=""
JOB_EXIT=""
JOB_ID=""
JOB_ERROR=""

usage() {
  cat <<'EOF'
Usage:
  IDLEMAX_BIN=/path/to/idlemax ./scripts/verify-linux.sh
  ./scripts/verify-linux.sh --self-test

Runs on Linux as a normal user. --self-test runs only the parser fixture tests
and needs no systemd, idlemax binary or Linux host.
EOF
}

die() {
  printf 'verify-linux: %s\n' "$*" >&2
  exit 2
}

record() {
  local ok=$1
  local name=$2
  local detail=$3
  detail=${detail//$'\t'/ }
  detail=${detail//$'\n'/ }
  CHECK_RESULTS+=("$ok")
  CHECK_NAMES+=("$name")
  CHECK_DETAILS+=("$detail")
  if [[ $ok != PASS ]]; then
    FAILED=1
  fi
}

print_summary() {
  local i
  if (( ${#CHECK_NAMES[@]} == 0 )); then
    return
  fi
  printf '\n%-4s  %-34s  %s\n' "RES" "CHECK" "DETAIL"
  printf '%-4s  %-34s  %s\n' "----" "----------------------------------" "------------------------------"
  for i in "${!CHECK_NAMES[@]}"; do
    printf '%-4s  %-34s  %s\n' "${CHECK_RESULTS[$i]}" "${CHECK_NAMES[$i]}" "${CHECK_DETAILS[$i]}"
  done
  if (( FAILED == 0 )); then
    if (( SELFTEST == 1 )); then
      printf '\nPASS: the parser fixtures matched every expected value.\n'
    else
      printf '\nPASS: every check matched the kernel and the systemd units verified clean.\n'
    fi
  else
    printf '\nFAIL: at least one check failed. See the rows above.\n'
  fi
}

cleanup() {
  set +e
  local unit
  # bash 3.2 (stock macOS) treats an empty array as unset under `set -u`.
  for unit in ${JOB_UNITS[@]+"${JOB_UNITS[@]}"}; do
    systemctl --user stop "$unit" >/dev/null 2>&1
  done
  if [[ -n $AGENT_UNIT ]]; then
    systemctl --user stop "$(scope_name "$AGENT_UNIT")" >/dev/null 2>&1
  fi
  if [[ -n $COORD_UNIT ]]; then
    systemctl --user stop "$(scope_name "$COORD_UNIT")" >/dev/null 2>&1
  fi
  if [[ -n $AGENT_PID ]]; then
    kill "$AGENT_PID" >/dev/null 2>&1
  fi
  if [[ -n $COORD_PID ]]; then
    kill "$COORD_PID" >/dev/null 2>&1
  fi
  if [[ -n $TMP && -d $TMP ]]; then
    if [[ ${IDLEMAX_VERIFY_KEEP:-0} == 1 ]]; then
      printf 'verify-linux: kept temp dir %s\n' "$TMP" >&2
    else
      rm -rf -- "$TMP"
    fi
  fi
}

finish() {
  local rc=$?
  trap - EXIT INT TERM
  cleanup
  print_summary
  if (( FAILED != 0 )); then
    rc=1
  fi
  exit "$rc"
}

trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

write_helper() {
  HELPER="$TMP/verify_linux_helper.py"
  cat > "$HELPER" <<'PY'
#!/usr/bin/env python3
"""Parsing and comparison helpers for verify-linux.sh (stdlib only)."""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

MIB = 1024 * 1024


def read_text(path):
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        return fh.read()


def read_int(path):
    try:
        return int(read_text(path).strip())
    except (OSError, ValueError):
        return None


def parse_meminfo(text):
    out = {}
    for line in text.splitlines():
        if ":" not in line:
            continue
        key, rest = line.split(":", 1)
        parts = rest.strip().split()
        if not parts:
            continue
        try:
            out[key] = int(parts[0]) * 1024
        except ValueError:
            continue
    return out


def parse_proc_stat(text):
    for line in text.splitlines():
        if line.startswith("cpu "):
            values = [int(v) for v in line.split()[1:]]
            # user nice system idle iowait irq softirq steal guest guest_nice
            total = sum(values)
            idle = values[3] + (values[4] if len(values) > 4 else 0)
            return total, total - idle
    return None


def parse_loadavg(text):
    return float(text.split()[0])


def parse_psi(text):
    out = {"some": None, "full": None}
    for line in text.splitlines():
        parts = line.split()
        if not parts:
            continue
        kind = parts[0]
        if kind not in out:
            continue
        for token in parts[1:]:
            if token.startswith("avg10="):
                try:
                    out[kind] = float(token.split("=", 1)[1])
                except ValueError:
                    pass
    return out


def read_psi(root, resource):
    path = os.path.join(root, "proc/pressure", resource)
    if not os.path.exists(path):
        return {"some": None, "full": None}
    return parse_psi(read_text(path))


def parse_vmstat_pswpin(text):
    for line in text.splitlines():
        if line.startswith("pswpin "):
            try:
                return int(line.split()[1])
            except (IndexError, ValueError):
                return None
    return None


def parse_df(text):
    lines = [l for l in text.splitlines() if l.strip()]
    if len(lines) < 2:
        return None
    parts = lines[1].split()
    cap = None
    for i in range(len(parts) - 1, -1, -1):
        if parts[i].endswith("%"):
            cap = i
            break
    if cap is None or cap < 3:
        return None
    try:
        return int(parts[cap - 1]) * 1024, int(parts[cap - 3]) * 1024
    except (IndexError, ValueError):
        return None


def parse_power(root):
    base = os.path.join(root, "sys/class/power_supply")
    try:
        entries = sorted(os.listdir(base))
    except OSError:
        return None, None
    mains_seen = False
    mains_online = False
    battery_pct = None
    status_known = False
    discharging = False
    for entry in entries:
        path = os.path.join(base, entry)
        try:
            kind = read_text(os.path.join(path, "type")).strip()
        except OSError:
            continue
        if kind in ("Mains", "USB"):
            mains_seen = True
            try:
                if read_text(os.path.join(path, "online")).strip() == "1":
                    mains_online = True
            except OSError:
                pass
        elif kind == "Battery":
            try:
                scope = read_text(os.path.join(path, "scope")).strip()
            except OSError:
                scope = ""
            if scope == "Device":
                continue
            if battery_pct is None:
                try:
                    battery_pct = float(read_text(os.path.join(path, "capacity")).strip())
                except (OSError, ValueError):
                    pass
            try:
                status = read_text(os.path.join(path, "status")).strip()
            except OSError:
                status = ""
            if status in ("Charging", "Discharging", "Not charging", "Full"):
                status_known = True
            if status == "Discharging":
                discharging = True
    if mains_seen:
        on_ac = mains_online
    elif battery_pct is not None and status_known:
        on_ac = not discharging
    else:
        on_ac = None
    return on_ac, battery_pct


def read_thermal(root):
    base = os.path.join(root, "sys/class/thermal")
    try:
        entries = sorted(os.listdir(base))
    except OSError:
        return None
    any_zone = False
    limited = False
    for entry in entries:
        if not entry.startswith("thermal_zone"):
            continue
        zone = os.path.join(base, entry)
        try:
            temp = int(read_text(os.path.join(zone, "temp")).strip())
        except (OSError, ValueError):
            continue
        any_zone = True
        i = 0
        while True:
            kind_path = os.path.join(zone, "trip_point_%d_type" % i)
            if not os.path.exists(kind_path):
                break
            try:
                kind = read_text(kind_path).strip()
            except OSError:
                break
            if kind in ("passive", "hot", "critical"):
                try:
                    trip = int(read_text(os.path.join(zone, "trip_point_%d_temp" % i)).strip())
                except (OSError, ValueError):
                    trip = None
                if trip is not None and trip > 0 and temp >= trip:
                    limited = True
            i += 1
    return limited if any_zone else None


def parse_logind(text, now_us):
    fields = {}
    for line in text.splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            fields[key.strip()] = value.strip()
    if fields.get("CanGraphical") != "yes":
        return None
    hint = fields.get("IdleHint")
    if hint is None:
        return None
    if hint == "yes":
        try:
            since = int(fields.get("IdleSinceHint", "0"))
        except ValueError:
            return None
        if since <= 0:
            return None
        return max(0, (now_us - since) // 1_000_000)
    return 0


def logind_expected(root, now_us):
    seat = os.path.join(root, "run/systemd/seats/seat0")
    if not os.path.exists(seat):
        return None
    try:
        result = subprocess.run(
            ["loginctl", "show-seat", "seat0", "-p", "CanGraphical",
             "-p", "IdleHint", "-p", "IdleSinceHint"],
            capture_output=True, text=True, timeout=3, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return parse_logind(result.stdout, now_us)


def read_pswpin(root="/"):
    path = os.path.join(root, "proc/vmstat")
    try:
        return parse_vmstat_pswpin(read_text(path))
    except OSError:
        return None


def df_root():
    try:
        result = subprocess.run(["/bin/df", "-kP", "/"], capture_output=True,
                                text=True, timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return parse_df(result.stdout)


def rows(checks):
    for ok, name, detail in checks:
        print("%s\t%s\t%s" % ("PASS" if ok else "FAIL", name, detail))


def close_enough(actual, expected, tol):
    return actual is not None and abs(actual - expected) <= tol


def check_metrics(args):
    prev = json.loads(read_text(args.prev))
    cur = json.loads(read_text(args.cur))
    pm = prev.get("metrics") or {}
    cm = cur.get("metrics") or {}
    checks = []
    now_ms = int(time.time() * 1000)
    root = args.root

    def add(ok, name, detail):
        checks.append((ok, name, detail))

    if not cm:
        add(False, "metrics.present", "the status has no metrics block")
        rows(checks)
        return 0

    age = now_ms - int(cm.get("at_ms", 0))
    add(-1000 <= age <= 5000, "metrics.fresh",
        "sample age %d ms (limit 5000)" % age)

    cpu_count = os.cpu_count()
    add(cm.get("cpu_count") == cpu_count, "metrics.cpu_count",
        "reported %s kernel %s" % (cm.get("cpu_count"), cpu_count))

    try:
        stat_now = read_text(os.path.join(root, "proc/stat"))
        load_now = parse_loadavg(read_text(os.path.join(root, "proc/loadavg")))
    except OSError as exc:
        add(False, "metrics.kernel_proc", "cannot read /proc: %s" % exc)
        rows(checks)
        return 0

    load_tol = max(0.2, abs(load_now) * 0.2)
    add(close_enough(cm.get("load_avg_1m"), load_now, load_tol), "metrics.load_1m",
        "agent %.2f kernel %.2f tol %.2f" % (cm.get("load_avg_1m") or 0.0, load_now, load_tol))

    if args.prev_stat and args.cur_stat:
        try:
            t0, b0 = parse_proc_stat(read_text(args.prev_stat))
            t1, b1 = parse_proc_stat(read_text(args.cur_stat))
            expected_cpu = ((b1 - b0) / float(t1 - t0)) * 100.0 if t1 > t0 else None
        except (OSError, TypeError, ZeroDivisionError):
            expected_cpu = None
        if expected_cpu is None:
            add(False, "metrics.cpu_busy_pct", "cannot compute the kernel CPU interval")
        else:
            actual_cpu = cm.get("cpu_busy_pct")
            add(close_enough(actual_cpu, expected_cpu, 25.0), "metrics.cpu_busy_pct",
                "agent %.2f%% kernel %.2f%% tol 25" % (actual_cpu or 0.0, expected_cpu))

    try:
        mem = parse_meminfo(read_text(os.path.join(root, "proc/meminfo")))
    except OSError as exc:
        add(False, "metrics.kernel_meminfo", "cannot read /proc/meminfo: %s" % exc)
        rows(checks)
        return 0

    add(cm.get("mem_total_bytes") == mem.get("MemTotal"), "metrics.mem_total",
        "agent %s kernel %s" % (cm.get("mem_total_bytes"), mem.get("MemTotal")))
    avail = mem.get("MemAvailable") or mem.get("MemFree") or 0
    add(close_enough(cm.get("mem_available_bytes"), avail, 64 * MIB),
        "metrics.mem_available",
        "agent %s kernel %s tol 64 MiB" % (cm.get("mem_available_bytes"), avail))
    expected_pct = (avail / float(mem.get("MemTotal", 1))) * 100.0
    add(close_enough(cm.get("mem_free_pct"), expected_pct, 1.0), "metrics.mem_free_pct",
        "agent %.2f%% kernel %.2f%% tol 1" % (cm.get("mem_free_pct") or 0.0, expected_pct))

    swap_total = mem.get("SwapTotal", 0)
    swap_used = swap_total - mem.get("SwapFree", 0)
    add(cm.get("swap_total_bytes") == swap_total, "metrics.swap_total",
        "agent %s kernel %s" % (cm.get("swap_total_bytes"), swap_total))
    add(close_enough(cm.get("swap_used_bytes"), swap_used, 64 * MIB),
        "metrics.swap_used",
        "agent %s kernel %s tol 64 MiB" % (cm.get("swap_used_bytes"), swap_used))

    disk = df_root()
    if disk is None:
        add(False, "metrics.disk", "cannot read df -kP /")
    else:
        free, total = disk
        add(cm.get("disk_total_bytes") == total, "metrics.disk_total",
            "agent %s kernel %s" % (cm.get("disk_total_bytes"), total))
        add(close_enough(cm.get("disk_free_bytes"), free, 128 * MIB), "metrics.disk_free",
            "agent %s kernel %s tol 128 MiB" % (cm.get("disk_free_bytes"), free))

    idle_expected = logind_expected(root, int(cm.get("at_ms", 0)) * 1000)
    idle_actual = cm.get("user_idle_secs")
    if idle_expected is None:
        add(idle_actual is None, "metrics.user_idle",
            "agent %s expected null (no graphical seat)" % idle_actual)
    elif idle_expected == 0:
        add(idle_actual == 0, "metrics.user_idle",
            "agent %s expected 0 (active seat)" % idle_actual)
    else:
        add(close_enough(idle_actual, idle_expected, 5), "metrics.user_idle",
            "agent %s expected %s tol 5 s" % (idle_actual, idle_expected))

    power_on, power_pct = parse_power(root)
    add(cm.get("on_ac_power") == power_on, "metrics.on_ac_power",
        "agent %s kernel %s" % (cm.get("on_ac_power"), power_on))
    if power_pct is None:
        add(cm.get("battery_pct") is None, "metrics.battery_pct",
            "agent %s expected null" % cm.get("battery_pct"))
    else:
        add(close_enough(cm.get("battery_pct"), power_pct, 0.05), "metrics.battery_pct",
            "agent %s kernel %s" % (cm.get("battery_pct"), power_pct))

    thermal = read_thermal(root)
    add(cm.get("thermal_limited") == thermal, "metrics.thermal_limited",
        "agent %s kernel %s" % (cm.get("thermal_limited"), thermal))
    add(cm.get("memory_pressure") is None, "metrics.memory_pressure",
        "agent %s expected null on Linux" % cm.get("memory_pressure"))

    psi_mem = read_psi(root, "memory")
    psi_io = read_psi(root, "io")
    for name, actual, expected in (
        ("metrics.psi_mem_some", cm.get("psi_mem_some_avg10"), psi_mem["some"]),
        ("metrics.psi_mem_full", cm.get("psi_mem_full_avg10"), psi_mem["full"]),
        ("metrics.psi_io_full", cm.get("psi_io_full_avg10"), psi_io["full"]),
    ):
        if expected is None:
            add(actual is None, name, "agent %s expected null" % actual)
        else:
            add(close_enough(actual, expected, 0.5), name,
                "agent %s kernel %.2f tol 0.5" % (actual, expected))

    cur_rate = cm.get("swap_in_per_sec")
    elapsed = (int(cm.get("at_ms", 0)) - int(pm.get("at_ms", 0))) / 1000.0
    p0 = args.prev_pswpin
    p1 = args.cur_pswpin
    if cur_rate is None or elapsed <= 0 or p0 is None or p1 is None or p1 < p0:
        add(False, "metrics.swap_in_per_sec", "rate was not computed across two samples")
    else:
        expected_rate = (p1 - p0) / elapsed
        tol = max(2.0, expected_rate * 0.5)
        add(close_enough(cur_rate, expected_rate, tol), "metrics.swap_in_per_sec",
            "agent %.2f pages/s kernel %.2f tol %.2f" % (cur_rate, expected_rate, tol))

    rows(checks)
    return 0


def has_metrics():
    data = json.load(sys.stdin)
    metrics = data.get("metrics")
    return 0 if metrics and int(metrics.get("at_ms", 0)) > 0 else 1


def sync_sample(binary, out, prev_at):
    """Waits for the agent to publish a sample newer than prev_at, then reads the kernel
    counters at once, so they cover the same window as the sample. Swap-in is bursty:
    counters read at any other moment can differ from the agent's rate by a whole window.
    Writes <out>.json and <out>.stat, prints "<at_ms> <pswpin>"."""
    deadline = time.monotonic() + 8.0
    while time.monotonic() < deadline:
        result = subprocess.run([binary, "local", "status", "--json"],
                                capture_output=True, text=True, timeout=10)
        if result.returncode == 0:
            try:
                at = int((json.loads(result.stdout).get("metrics") or {}).get("at_ms", 0))
            except ValueError:
                at = 0
            if at and at != prev_at:
                pswpin = read_pswpin("/")
                if pswpin is None:
                    print("cannot read /proc/vmstat", file=sys.stderr)
                    return 1
                with open(out + ".json", "w") as fh:
                    fh.write(result.stdout)
                with open(out + ".stat", "w") as fh:
                    fh.write(read_text("/proc/stat"))
                print("%d %d" % (at, pswpin))
                return 0
        time.sleep(0.05)
    print("the agent published no new metrics sample within 8 s", file=sys.stderr)
    return 1


def job_info(path, job_id=None):
    data = json.loads(read_text(path))
    job = None
    if isinstance(data, list):
        for item in data:
            if str(item.get("id")) == str(job_id):
                job = item
                break
    elif job_id is None or str(data.get("id")) == str(job_id):
        job = data
    if job is None:
        return 1
    error = (job.get("last_error") or "").replace("\t", " ").replace("\n", " ")
    print("\t".join([
        str(job.get("id")),
        str(job.get("status")),
        "-" if job.get("exit_code") is None else str(job.get("exit_code")),
        error,
    ]))
    return 0


def cluster_find(path, device_id):
    data = json.loads(read_text(path))
    for item in data.get("devices") or []:
        info = item.get("info") or {}
        if info.get("id") == device_id:
            return item
    return None


def cluster_ok(path, device_id):
    found = cluster_find(path, device_id)
    if found is None:
        return 1
    age = int(time.time() * 1000) - int(found.get("last_seen_ms", 0))
    return 0 if found.get("online") and age <= 5000 and found.get("metrics") else 1


def cluster_check(path, device_id):
    checks = []
    found = cluster_find(path, device_id)
    checks.append((found is not None, "cluster.device",
                   "device %s %s" % (device_id, "registered" if found else "missing")))
    if found is not None:
        age = int(time.time() * 1000) - int(found.get("last_seen_ms", 0))
        checks.append((bool(found.get("online")) and age <= 5000,
                       "cluster.heartbeat",
                       "online=%s last_seen=%d ms ago" % (found.get("online"), age)))
        checks.append((bool(found.get("metrics")), "cluster.metrics",
                       "coordinator received a metrics snapshot"))
    rows(checks)
    return 0


def scope_check(base, mem_mb, high_mb, cpu_us):
    checks = []
    expected_max = mem_mb * MIB
    expected_high = high_mb * MIB
    actual_max = read_int(os.path.join(base, "memory.max"))
    actual_high = read_int(os.path.join(base, "memory.high"))
    actual_swap = read_int(os.path.join(base, "memory.swap.max"))
    try:
        actual_cpu = " ".join(read_text(os.path.join(base, "cpu.max")).split())
    except OSError:
        actual_cpu = None
    expected_cpu = "%d 100000" % cpu_us
    checks.append((actual_max == expected_max, "memory.max",
                   "actual %s expected %s" % (actual_max, expected_max)))
    checks.append((actual_high == expected_high, "memory.high",
                   "actual %s expected %s" % (actual_high, expected_high)))
    checks.append((actual_swap == 0, "memory.swap.max",
                   "actual %s expected 0" % actual_swap))
    checks.append((actual_cpu == expected_cpu, "cpu.max",
                   "actual %r expected %r" % (actual_cpu, expected_cpu)))
    rows(checks)
    return 0


def parse_memory_events(text):
    """memory.events lines are "<name> <count>"."""
    counts = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].isdigit():
            counts[parts[0]] = int(parts[1])
    return counts


def watch_cgroup(base, out_path, timeout=90.0):
    end = time.monotonic() + timeout
    samples = 0
    missing = 0
    max_current = 0
    peak = 0
    oom_kill = 0
    memory_max = None
    swap_max = None
    max_swap = 0
    while time.monotonic() < end:
        if not os.path.isdir(base):
            missing += 1
            if missing > 40:
                break
            time.sleep(0.01)
            continue
        missing = 0
        samples += 1
        current = read_int(os.path.join(base, "memory.current"))
        if current is not None:
            max_current = max(max_current, current)
        events = None
        try:
            events = read_text(os.path.join(base, "memory.events"))
        except OSError:
            pass
        if events:
            counts = parse_memory_events(events)
            oom_kill = max(oom_kill, counts.get("oom_kill", 0))
        peak_value = read_int(os.path.join(base, "memory.peak"))
        if peak_value is not None:
            peak = max(peak, peak_value)
        memory_max = read_int(os.path.join(base, "memory.max")) or memory_max
        swap_max = read_int(os.path.join(base, "memory.swap.max")) or swap_max
        swap_current = read_int(os.path.join(base, "memory.swap.current"))
        if swap_current is not None:
            max_swap = max(max_swap, swap_current)
        time.sleep(0.001)
    data = {
        "samples": samples,
        "max_current": max_current,
        "peak": max(peak, max_current),
        "oom_kill": oom_kill,
        "memory_max": memory_max,
        "swap_max": swap_max,
        "max_swap": max_swap,
    }
    with open(out_path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, sort_keys=True)
    return 0


def watch_info(path):
    data = json.loads(read_text(path))
    print("\t".join([
        str(data.get("samples", 0)),
        str(data.get("max_current", 0)),
        str(data.get("peak", 0)),
        str(data.get("oom_kill", 0)),
        str(data.get("memory_max")),
        str(data.get("swap_max")),
        str(data.get("max_swap", 0)),
    ]))
    return 0


def cpu_value(usage0, usage1, elapsed_ns):
    elapsed_us = elapsed_ns / 1000.0
    if elapsed_us <= 0:
        return None
    return ((usage1 - usage0) / elapsed_us) * 100.0


def cpu_percent(usage0, usage1, elapsed_ns):
    value = cpu_value(usage0, usage1, elapsed_ns)
    if value is None:
        return 1
    print("%.2f" % value)
    return 0


def monotonic_ns():
    print(time.monotonic_ns())
    return 0


def http_ok(url):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            body = response.read(16).decode("utf-8", "replace")
            return 0 if response.status == 200 and body.strip() == "ok" else 1
    except Exception:
        return 1


def pick_ports():
    socks = []
    ports = []
    try:
        for _ in range(2):
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.bind(("127.0.0.1", 0))
            socks.append(sock)
            ports.append(sock.getsockname()[1])
    finally:
        for sock in socks:
            sock.close()
    print("%d %d" % (ports[0], ports[1]))
    return 0


def selftest():
    checks = []
    tmp = tempfile.mkdtemp(prefix="idlemax-selftest-")
    try:
        def add(ok, name, detail=""):
            checks.append((ok, name, detail))

        mem = parse_meminfo(
            "MemTotal:       32948684 kB\n"
            "MemFree:         1234567 kB\n"
            "MemAvailable:   23456789 kB\n"
            "SwapTotal:       8388604 kB\n"
            "SwapFree:        4194304 kB\n"
        )
        add(mem["MemTotal"] == 32948684 * 1024, "parse_meminfo.total", str(mem.get("MemTotal")))
        add(mem["MemAvailable"] == 23456789 * 1024, "parse_meminfo.available", str(mem.get("MemAvailable")))
        add(mem["SwapTotal"] - mem["SwapFree"] == 4194300 * 1024, "parse_meminfo.swap_used", "4194300 KiB")

        stat0 = "cpu  10 0 10 80 0 0 0 0 0 0\ncpu0 1 2 3 4\n"
        stat1 = "cpu  20 0 20 160 0 0 0 0 0 0\ncpu0 1 2 3 4\n"
        t0, b0 = parse_proc_stat(stat0)
        t1, b1 = parse_proc_stat(stat1)
        add((t0, b0, t1, b1) == (100, 20, 200, 40), "parse_proc_stat",
            "total/busy %s/%s then %s/%s" % (t0, b0, t1, b1))

        add(parse_loadavg("0.42 0.31 0.22 1/100 1234\n") == 0.42,
            "parse_loadavg", "0.42")

        psi = parse_psi("some avg10=1.25 avg60=0.50 avg300=0.10 total=1234\n"
                        "full avg10=0.75 avg60=0.25 avg300=0.05 total=5678\n")
        add(psi == {"some": 1.25, "full": 0.75}, "parse_psi", repr(psi))

        add(parse_vmstat_pswpin("nr_free_pages 100\npswpout 40\npswpin 123456\n") == 123456,
            "parse_vmstat_pswpin", "123456")

        # Root "/" must read the absolute /proc, never a proc/ under the current directory.
        decoy = os.path.join(tmp, "decoy")
        os.makedirs(os.path.join(decoy, "proc"))
        with open(os.path.join(decoy, "proc/vmstat"), "w") as fh:
            fh.write("pswpin 987654321987\n")
        here = os.getcwd()
        os.chdir(decoy)
        try:
            from_root = read_pswpin("/")
        finally:
            os.chdir(here)
        add(from_root != 987654321987, "read_pswpin.root_slash", repr(from_root))

        # A job with no exit code must not leave an empty tab field: bash would collapse it
        # and shift the error text into the exit column.
        import io
        jobs = os.path.join(tmp, "jobs.json")
        with open(jobs, "w") as fh:
            json.dump([{"id": 7, "status": "parked", "exit_code": None,
                        "last_error": "killed by the 128 MiB memory cap"}], fh)
        captured, real_stdout = io.StringIO(), sys.stdout
        sys.stdout = captured
        try:
            job_info(jobs, "7")
        finally:
            sys.stdout = real_stdout
        fields = captured.getvalue().rstrip("\n").split("\t")
        add(fields == ["7", "parked", "-", "killed by the 128 MiB memory cap"],
            "job_info.no_exit_code", repr(fields))

        df = parse_df("Filesystem 1024-blocks Used Available Capacity Mounted on\n"
                      "/dev/sda1 100000 20000 80000 20% /\n")
        add(df == (80000 * 1024, 100000 * 1024), "parse_df", repr(df))

        power = os.path.join(tmp, "power")
        os.makedirs(os.path.join(power, "sys/class/power_supply/AC"))
        os.makedirs(os.path.join(power, "sys/class/power_supply/BAT0"))
        os.makedirs(os.path.join(power, "sys/class/power_supply/mouse"))
        with open(os.path.join(power, "sys/class/power_supply/AC/type"), "w") as fh:
            fh.write("Mains\n")
        with open(os.path.join(power, "sys/class/power_supply/AC/online"), "w") as fh:
            fh.write("1\n")
        with open(os.path.join(power, "sys/class/power_supply/BAT0/type"), "w") as fh:
            fh.write("Battery\n")
        with open(os.path.join(power, "sys/class/power_supply/BAT0/capacity"), "w") as fh:
            fh.write("41\n")
        with open(os.path.join(power, "sys/class/power_supply/BAT0/status"), "w") as fh:
            fh.write("Discharging\n")
        with open(os.path.join(power, "sys/class/power_supply/mouse/type"), "w") as fh:
            fh.write("Battery\n")
        with open(os.path.join(power, "sys/class/power_supply/mouse/scope"), "w") as fh:
            fh.write("Device\n")
        with open(os.path.join(power, "sys/class/power_supply/mouse/capacity"), "w") as fh:
            fh.write("99\n")
        add(parse_power(power) == (True, 41.0), "parse_power.mains", repr(parse_power(power)))

        unknown = os.path.join(tmp, "unknown-power")
        os.makedirs(os.path.join(unknown, "sys/class/power_supply/BAT0"))
        with open(os.path.join(unknown, "sys/class/power_supply/BAT0/type"), "w") as fh:
            fh.write("Battery\n")
        with open(os.path.join(unknown, "sys/class/power_supply/BAT0/capacity"), "w") as fh:
            fh.write("55\n")
        with open(os.path.join(unknown, "sys/class/power_supply/BAT0/status"), "w") as fh:
            fh.write("Unknown\n")
        add(parse_power(unknown) == (None, 55.0), "parse_power.unknown_status",
            repr(parse_power(unknown)))

        thermal = os.path.join(tmp, "thermal")
        zone = os.path.join(thermal, "sys/class/thermal/thermal_zone0")
        os.makedirs(zone)
        with open(os.path.join(zone, "temp"), "w") as fh:
            fh.write("85000\n")
        with open(os.path.join(zone, "trip_point_0_type"), "w") as fh:
            fh.write("active\n")
        with open(os.path.join(zone, "trip_point_0_temp"), "w") as fh:
            fh.write("70000\n")
        with open(os.path.join(zone, "trip_point_1_type"), "w") as fh:
            fh.write("passive\n")
        with open(os.path.join(zone, "trip_point_1_temp"), "w") as fh:
            fh.write("90000\n")
        with open(os.path.join(zone, "trip_point_2_type"), "w") as fh:
            fh.write("critical\n")
        with open(os.path.join(zone, "trip_point_2_temp"), "w") as fh:
            fh.write("0\n")
        add(read_thermal(thermal) is False, "read_thermal.under_trip", "false")
        with open(os.path.join(zone, "temp"), "w") as fh:
            fh.write("95000\n")
        add(read_thermal(thermal) is True, "read_thermal.over_passive", "true")
        add(read_thermal(os.path.join(tmp, "missing")) is None, "read_thermal.no_zones", "null")

        logind = ("CanGraphical=yes\nIdleHint=yes\nIdleSinceHint=1000000\n")
        add(parse_logind(logind, 11_000_000) == 10, "parse_logind.idle", "10 s")
        add(parse_logind("CanGraphical=no\n", 11_000_000) is None, "parse_logind.headless", "null")
        add(parse_logind("CanGraphical=yes\nIdleHint=no\n", 11_000_000) == 0,
            "parse_logind.active", "0")

        cgroup = os.path.join(tmp, "cgroup")
        os.makedirs(cgroup)
        with open(os.path.join(cgroup, "memory.max"), "w") as fh:
            fh.write("67108864\n")
        with open(os.path.join(cgroup, "cpu.max"), "w") as fh:
            fh.write("100000 100000\n")
        add(read_int(os.path.join(cgroup, "memory.max")) == 64 * MIB,
            "read_cgroup.memory_max", "64 MiB")

        with open(os.path.join(cgroup, "memory.events"), "w") as fh:
            fh.write("low 0\nhigh 7\nmax 2\noom 1\noom_kill 1\noom_group_kill 0\n")
        watch = os.path.join(tmp, "watch.json")
        watch_cgroup(cgroup, watch, timeout=0.05)
        info = json.loads(read_text(watch))
        add(info["memory_max"] == 64 * MIB and info["samples"] > 0,
            "watch_cgroup", "samples=%s max=%s" % (info["samples"], info["memory_max"]))
        add(info.get("oom_kill") == 1,
            "watch_cgroup.events", "oom_kill=%s" % info.get("oom_kill"))

        add(close_enough(cpu_value(1000, 2000, 1_000_000_000), 0.1, 0.001),
            "cpu_percent", "0.1")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    bad = [c for c in checks if not c[0]]
    for ok, name, detail in checks:
        print("%s  %-32s %s" % ("ok" if ok else "FAIL", name, detail))
    if bad:
        print("self-test: %d of %d checks failed" % (len(bad), len(checks)))
        return 1
    print("self-test: %d checks passed" % len(checks))
    return 0


class Args:
    pass


def parse_args(argv):
    args = Args()
    args.prev = None
    args.cur = None
    args.prev_pswpin = None
    args.cur_pswpin = None
    args.prev_stat = None
    args.cur_stat = None
    args.root = "/"
    i = 0
    while i < len(argv):
        key = argv[i]
        if key in ("--prev", "--cur", "--prev-stat", "--cur-stat", "--root"):
            if i + 1 >= len(argv):
                raise SystemExit("missing value for %s" % key)
            setattr(args, key[2:].replace("-", "_"), argv[i + 1])
            i += 2
        elif key in ("--prev-pswpin", "--cur-pswpin"):
            if i + 1 >= len(argv):
                raise SystemExit("missing value for %s" % key)
            setattr(args, key[2:].replace("-", "_"), int(argv[i + 1]))
            i += 2
        else:
            raise SystemExit("unknown option %s" % key)
    return args


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    cmd = argv[0]
    rest = argv[1:]
    if cmd == "selftest":
        return selftest()
    if cmd == "has-metrics":
        return has_metrics()
    if cmd == "sync-sample":
        return sync_sample(rest[0], rest[1], int(rest[2]))
    if cmd == "job-info":
        return job_info(rest[0], rest[1] if len(rest) > 1 else None)
    if cmd == "cluster-check":
        return cluster_check(rest[0], rest[1])
    if cmd == "cluster-ok":
        return cluster_ok(rest[0], rest[1])
    if cmd == "check-metrics":
        return check_metrics(parse_args(rest))
    if cmd == "scope-check":
        return scope_check(rest[0], int(rest[1]), int(rest[2]), int(rest[3]))
    if cmd == "watch-cgroup":
        return watch_cgroup(rest[0], rest[1], float(rest[2]) if len(rest) > 2 else 90.0)
    if cmd == "watch-info":
        return watch_info(rest[0])
    if cmd == "cpu-percent":
        return cpu_percent(int(rest[0]), int(rest[1]), int(rest[2]))
    if cmd == "monotonic-ns":
        return monotonic_ns()
    if cmd == "http-ok":
        return http_ok(rest[0])
    if cmd == "pick-ports":
        return pick_ports()
    print("unknown helper command %s" % cmd, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
PY
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

run_idle() {
  env -u IDLEMAX_COORD -u IDLEMAX_TOKEN \
    IDLEMAX_HOME="$STATE" HOME="$FAKE_HOME" "$IDLEMAX_BIN" "$@"
}

local_status_json() {
  run_idle local status --json
}

http_ok() {
  python3 "$HELPER" http-ok "$1"
}

wait_until() {
  local timeout=$1
  shift
  local end=$((SECONDS + timeout))
  while (( SECONDS < end )); do
    if "$@"; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

# Split the helper's "coord agent" line into the two globals. The script's IFS is
# newline and tab, so the space has to be named here or the whole line lands in one.
split_ports() {
  local ports=$1
  IFS=' ' read -r COORD_PORT AGENT_PORT <<< "$ports"
  [[ -n ${COORD_PORT:-} && -n ${AGENT_PORT:-} && ${COORD_PORT:-} =~ ^[0-9]+$ && ${AGENT_PORT:-} =~ ^[0-9]+$ ]]
}

local_ready() {
  local json
  json=$(local_status_json 2>/dev/null) || return 1
  printf '%s' "$json" | python3 "$HELPER" has-metrics
}

start_scope() {
  local unit=$1
  local mem_high=$2
  local mem_max=$3
  local cpu_quota=$4
  local nice=$5
  local log=$6
  shift 6
  systemd-run --user --scope --quiet --collect --expand-environment=no \
    --unit="$unit" --nice="$nice" \
    --property=MemoryHigh="$mem_high" \
    --property=MemoryMax="$mem_max" \
    --property=MemorySwapMax=0 \
    --property=CPUQuota="$cpu_quota" \
    --property=CPUWeight=10 \
    --property=IOWeight=10 \
    --setenv=IDLEMAX_HOME="$STATE" \
    --setenv=HOME="$FAKE_HOME" \
    --setenv=RUST_LOG=warn \
    -- "$@" >"$log" 2>&1 &
  printf '%s' "$!"
}

# systemctl reads a bare name as a .service, but these units are scopes.
scope_name() {
  if [[ $1 == *.scope ]]; then
    printf '%s' "$1"
  else
    printf '%s.scope' "$1"
  fi
}

scope_cgroup() {
  local unit
  unit=$(scope_name "$1")
  local cg
  cg=$(systemctl --user show -p ControlGroup --value "$unit" 2>/dev/null) || return 1
  cg=${cg//$'\n'/}
  [[ -n $cg && $cg != "/" ]] || return 1
  [[ $cg == /* ]] || cg="/$cg"
  printf '/sys/fs/cgroup%s' "$cg"
}

check_caps() {
  local label=$1
  local unit=$2
  local mem_mb=$3
  local high_mb=$4
  local cpu_us=$5
  local base
  if ! base=$(scope_cgroup "$unit") || [[ ! -d $base ]]; then
    record FAIL "caps.$label" "no cgroup for $unit"
    return
  fi
  local out="$TMP/caps.$label.tsv"
  if ! python3 "$HELPER" scope-check "$base" "$mem_mb" "$high_mb" "$cpu_us" > "$out"; then
    record FAIL "caps.$label" "cannot read cgroup properties in $base"
    return
  fi
  local ok name detail
  while IFS=$'\t' read -r ok name detail; do
    record "$ok" "caps.$label.$name" "$detail"
  done < "$out"
}

job_refresh() {
  local id=$1
  if ! run_idle jobs --json > "$TMP/jobs.json" 2>"$TMP/jobs.err"; then
    return 1
  fi
  local line
  if ! line=$(python3 "$HELPER" job-info "$TMP/jobs.json" "$id"); then
    return 1
  fi
  IFS=$'\t' read -r JOB_ID JOB_STATUS JOB_EXIT JOB_ERROR <<< "$line"
  [[ $JOB_EXIT != - ]] || JOB_EXIT=""
}

wait_job_running() {
  local id=$1
  local timeout=$2
  local end=$((SECONDS + timeout))
  while (( SECONDS < end )); do
    if job_refresh "$id"; then
      case "$JOB_STATUS" in
        running|paused|leased) return 0 ;;
        succeeded|failed|killed|cancelled|parked) return 2 ;;
      esac
    fi
    sleep 0.2
  done
  return 1
}

wait_job_terminal() {
  local id=$1
  local timeout=$2
  local end=$((SECONDS + timeout))
  while (( SECONDS < end )); do
    if job_refresh "$id"; then
      case "$JOB_STATUS" in
        succeeded|failed|killed|cancelled|parked) return 0 ;;
      esac
    fi
    sleep 0.5
  done
  return 1
}

find_job_scope() {
  local id=$1
  local timeout=$2
  local end=$((SECONDS + timeout))
  while (( SECONDS < end )); do
    local unit
    unit=$(systemctl --user --no-pager --no-legend --plain --type=scope --all \
      list-units 'idlemax-job-*.scope' 2>/dev/null \
      | awk -v prefix="idlemax-job-$id-" '$1 ~ ("^" prefix) { print $1; exit }')
    if [[ -n $unit ]]; then
      printf '%s' "$unit"
      return 0
    fi
    sleep 0.05
  done
  return 1
}

write_job_spec() {
  local path=$1
  local name=$2
  local command=$3
  local mem_mb=$4
  local runtime=$5
  cat > "$path" <<EOF
name = "$name"
command = ["$PYTHON_BIN", "$command"]
cost = "cpu_light"
mem_mb = $mem_mb
max_runtime_secs = $runtime
max_attempts = 1
on_user_active = "continue"
EOF
}

add_job() {
  local spec=$1
  if ! run_idle job add "$spec" --json > "$TMP/job.add.json" 2>"$TMP/job.add.err"; then
    return 1
  fi
  IFS=$'\t' read -r JOB_ID JOB_STATUS JOB_EXIT JOB_ERROR \
    < <(python3 "$HELPER" job-info "$TMP/job.add.json")
  [[ $JOB_EXIT != - ]] || JOB_EXIT=""
  [[ -n ${JOB_ID:-} ]]
}

check_memory_job() {
  local spec="$TMP/job-memory.toml"
  local hog="$TMP/mem_hog.py"
  cat > "$hog" <<'PY'
import sys
import time

chunks = []
for _ in range(4096):
    chunks.append(bytearray(1024 * 1024))
    time.sleep(0.010)
print("unexpected: memory hog reached the end", flush=True)
sys.exit(0)
PY
  write_job_spec "$spec" "verify-memory-hog" "$hog" 128 20
  if ! add_job "$spec"; then
    record FAIL "job.memory.add" "$(tail -n 2 "$TMP/job.add.err" 2>/dev/null)"
    return
  fi
  local id=$JOB_ID
  local rc=0
  wait_job_running "$id" 30 || rc=$?
  if (( rc != 0 )); then
    job_refresh "$id" || true
    record FAIL "job.memory.start" "status=${JOB_STATUS:-unknown} error=${JOB_ERROR:-none}"
    return
  fi
  local unit
  if ! unit=$(find_job_scope "$id" 20); then
    record FAIL "job.memory.scope" "no systemd scope for job $id"
    return
  fi
  JOB_UNITS+=("$unit")
  local base
  if ! base=$(scope_cgroup "$unit") || [[ ! -d $base ]]; then
    record FAIL "job.memory.cgroup" "no cgroup for $unit"
    return
  fi
  local watch="$TMP/mem_watch.json"
  python3 "$HELPER" watch-cgroup "$base" "$watch" 90 &
  local watcher=$!
  if ! wait_job_terminal "$id" 60; then
    record FAIL "job.memory.end" "job did not finish within 60 s"
    kill "$watcher" >/dev/null 2>&1 || true
    wait "$watcher" 2>/dev/null || true
    return
  fi
  for _ in $(seq 1 50); do
    kill -0 "$watcher" >/dev/null 2>&1 || break
    sleep 0.1
  done
  kill "$watcher" >/dev/null 2>&1 || true
  wait "$watcher" 2>/dev/null || true

  if [[ ! -s $watch ]]; then
    record FAIL "job.memory.watch" "the cgroup watcher produced no data"
  else
    local samples current peak oom memmax swapmax maxswap
    IFS=$'\t' read -r samples current peak oom memmax swapmax maxswap \
      < <(python3 "$HELPER" watch-info "$watch")
    local expected=$((128 * 1024 * 1024))
    record "$([[ $memmax == "$expected" ]] && printf PASS || printf FAIL)" \
      "job.memory.max" "memory.max=$memmax expected=$expected"
    record "$([[ $peak -le $expected ]] && printf PASS || printf FAIL)" \
      "job.memory.contained" "peak=$peak never above memory.max=$expected (samples=$samples, swap=$maxswap/$swapmax)"
    record "$([[ $peak -ge $((expected * 9 / 10)) ]] && printf PASS || printf FAIL)" \
      "job.memory.peak" "peak=$peak current=$current of $expected (90% required)"
    record "$([[ $oom -ge 1 ]] && printf PASS || printf FAIL)" \
      "job.memory.oom" "cgroup oom_kill=$oom"
    # No MemoryHigh below the cap: the kernel kills the hog at MemoryMax instead of throttling
    # it, and the agent reports that cap as the reason. The job has one attempt, so the
    # coordinator shows it as parked with that error.
    record "$([[ ( $JOB_STATUS == killed || $JOB_STATUS == parked ) && $JOB_ERROR == *"killed by the 128 MiB memory cap"* ]] && printf PASS || printf FAIL)" \
      "job.memory.killed" "status=$JOB_STATUS exit=${JOB_EXIT:-none} error=${JOB_ERROR:-none}"
  fi
}

check_cpu_job() {
  local spec="$TMP/job-cpu.toml"
  local burn="$TMP/cpu_burn.py"
  cat > "$burn" <<'PY'
import os
import time

def burn(until):
    value = 1
    while time.time() < until:
        value = (value * 1103515245 + 12345) & 0x7fffffff

workers = max(2, (os.cpu_count() or 2) * 2)
end = time.time() + 15.0
pids = []
for _ in range(workers):
    pid = os.fork()
    if pid == 0:
        burn(end)
        os._exit(0)
    pids.append(pid)
time.sleep(15.2)
for pid in pids:
    try:
        os.waitpid(pid, 0)
    except OSError:
        pass
PY
  write_job_spec "$spec" "verify-cpu-bound" "$burn" 256 120
  if ! add_job "$spec"; then
    record FAIL "job.cpu.add" "$(tail -n 2 "$TMP/job.add.err" 2>/dev/null)"
    return
  fi
  local id=$JOB_ID
  local rc=0
  wait_job_running "$id" 30 || rc=$?
  if (( rc != 0 )); then
    job_refresh "$id" || true
    record FAIL "job.cpu.start" "status=${JOB_STATUS:-unknown} error=${JOB_ERROR:-none}"
    return
  fi
  local unit
  if ! unit=$(find_job_scope "$id" 20); then
    record FAIL "job.cpu.scope" "no systemd scope for job $id"
    return
  fi
  JOB_UNITS+=("$unit")
  local base
  if ! base=$(scope_cgroup "$unit") || [[ ! -d $base ]]; then
    record FAIL "job.cpu.cgroup" "no cgroup for $unit"
    return
  fi

  local cpu_max
  cpu_max=$(tr -s ' ' < "$base/cpu.max" 2>/dev/null || true)
  record "$([[ $cpu_max == "100000 100000" ]] && printf PASS || printf FAIL)" \
    "job.cpu.quota" "cpu.max='$cpu_max' expected='100000 100000'"

  local usage0 usage1 throttled0 throttled1 t0 t1 observed throttled
  usage0=$(awk '$1 == "usage_usec" { print $2 }' "$base/cpu.stat")
  throttled0=$(awk '$1 == "nr_throttled" { print $2 }' "$base/cpu.stat")
  t0=$(python3 "$HELPER" monotonic-ns)
  sleep 5
  if [[ ! -d $base ]]; then
    record FAIL "job.cpu.measure" "the job scope disappeared before the 5 s sample"
    return
  fi
  usage1=$(awk '$1 == "usage_usec" { print $2 }' "$base/cpu.stat")
  throttled1=$(awk '$1 == "nr_throttled" { print $2 }' "$base/cpu.stat")
  t1=$(python3 "$HELPER" monotonic-ns)
  observed=$(python3 "$HELPER" cpu-percent "$usage0" "$usage1" "$((t1 - t0))")
  throttled=$((throttled1 - throttled0))
  record "$(awk -v v="$observed" 'BEGIN { print (v <= 115.0) ? "PASS" : "FAIL" }')" \
    "job.cpu.percent" "observed ${observed}% of one CPU (quota 100%, limit 115%)"
  record "$([[ $throttled -gt 0 ]] && printf PASS || printf FAIL)" \
    "job.cpu.throttled" "nr_throttled increased by $throttled"

  if ! wait_job_terminal "$id" 130; then
    record FAIL "job.cpu.end" "job did not finish within 130 s"
    return
  fi
  record "$([[ $JOB_STATUS == succeeded && ${JOB_EXIT:-} == 0 ]] && printf PASS || printf FAIL)" \
    "job.cpu.completed" "status=$JOB_STATUS exit=${JOB_EXIT:-none} error=${JOB_ERROR:-none}"
}

verify_units() {
  local units_dir=${IDLEMAX_UNITS_DIR:-$REPO_DIR/packaging/linux}
  local coord="$units_dir/idlemax-coord.service"
  local agent="$units_dir/idlemax-agent.service"
  if [[ ! -f $coord || ! -f $agent ]]; then
    record FAIL "units.files" "missing $coord or $agent"
    return
  fi
  local outdir="$TMP/units"
  mkdir -p "$outdir"
  local user group
  user=$(id -un)
  group=$(id -gn)
  sed -e "s|^ExecStart=.*|ExecStart=$IDLEMAX_BIN coord|" \
      -e "s|^User=.*|User=$user|" \
      -e "s|^Group=.*|Group=$group|" \
      "$coord" > "$outdir/idlemax-coord.service"
  sed -e "s|^ExecStart=.*|ExecStart=$IDLEMAX_BIN agent|" \
      -e "s|^User=.*|User=$user|" \
      -e "s|^Group=.*|Group=$group|" \
      "$agent" > "$outdir/idlemax-agent.service"
  local out="$TMP/units.verify.out"
  if systemd-analyze verify "$outdir/idlemax-coord.service" "$outdir/idlemax-agent.service" \
      > "$out" 2>&1; then
    if [[ -s $out ]]; then
      record FAIL "units.verify" "systemd-analyze verify printed: $(head -n 3 "$out" | tr '\n' ' ')"
    else
      record PASS "units.verify" "systemd-analyze verify: clean (ExecStart/User/Group substituted)"
    fi
  else
    record FAIL "units.verify" "systemd-analyze verify failed: $(head -n 3 "$out" | tr '\n' ' ')"
  fi
}

run_self_test() {
  SELFTEST=1
  TMP=$(mktemp -d "${TMPDIR:-/tmp}/idlemax-verify-selftest.XXXXXX")
  write_helper
  local out="$TMP/selftest.out"
  if python3 "$HELPER" selftest > "$out" 2>&1; then
    record PASS "selftest.parsers" "$(tail -n 1 "$out")"
  else
    record FAIL "selftest.parsers" "$(tail -n 3 "$out" | tr '\n' ' ')"
    cat "$out" >&2
  fi
  if split_ports "1234 5678" && [[ $COORD_PORT == 1234 && $AGENT_PORT == 5678 ]]; then
    record PASS "selftest.ports" "split 1234 5678 into two ports"
  else
    record FAIL "selftest.ports" "got '${COORD_PORT:-}' and '${AGENT_PORT:-}'"
  fi
  if [[ $(scope_name idlemax-verify-1-coord) == idlemax-verify-1-coord.scope \
      && $(scope_name idlemax-job-2-3.scope) == idlemax-job-2-3.scope ]]; then
    record PASS "selftest.scope_name" "scope units are always addressed with the .scope suffix"
  else
    record FAIL "selftest.scope_name" "got '$(scope_name idlemax-verify-1-coord)'"
  fi
}

# Waits for the agent's next sample and reads the kernel counters right after it (see
# sync_sample in the helper). Prints "<at_ms> <pswpin>". Runs with the agent's environment.
sync_sample() {
  env -u IDLEMAX_COORD -u IDLEMAX_TOKEN IDLEMAX_HOME="$STATE" HOME="$FAKE_HOME" \
    python3 "$HELPER" sync-sample "$IDLEMAX_BIN" "$1" "$2"
}

check_signals() {
  local status_dir="$TMP/metrics"
  mkdir -p "$status_dir"
  # Two samples one period apart, each read in step with the agent. The first call only
  # learns the current sample; it may be old, so the pair starts at the next one.
  local line at0 psw0 psw1
  if ! line=$(sync_sample "$status_dir/warm" 0 2>"$status_dir/status.err"); then
    record FAIL "metrics.status" "$(tail -n 2 "$status_dir/status.err" | tr '\n' ' ')"
    return 0
  fi
  IFS=' ' read -r at0 _ <<< "$line"
  if ! line=$(sync_sample "$status_dir/prev" "$at0" 2>"$status_dir/status.err"); then
    record FAIL "metrics.samples" "$(tail -n 2 "$status_dir/status.err" | tr '\n' ' ')"
    return 0
  fi
  IFS=' ' read -r at0 psw0 <<< "$line"
  if ! line=$(sync_sample "$status_dir/cur" "$at0" 2>"$status_dir/status.err"); then
    record FAIL "metrics.samples" "$(tail -n 2 "$status_dir/status.err" | tr '\n' ' ')"
    return 0
  fi
  IFS=" " read -r _ psw1 <<< "$line"

  local metrics_out="$TMP/metrics.tsv"
  if python3 "$HELPER" check-metrics \
      --prev "$status_dir/prev.json" --cur "$status_dir/cur.json" \
      --prev-pswpin "$psw0" --cur-pswpin "$psw1" \
      --prev-stat "$status_dir/prev.stat" --cur-stat "$status_dir/cur.stat" \
      --root / > "$metrics_out"; then
    local ok name detail
    while IFS=$'\t' read -r ok name detail; do
      record "$ok" "signal.$name" "$detail"
    done < "$metrics_out"
  else
    record FAIL "metrics.check" "the metrics helper failed"
  fi
}

# Everything that needs the coordinator and agent running. A failed step records FAIL and
# stops this function only, so main still runs verify_units.
live_checks() {
  local probe_unit="idlemax-verify-$$-probe"
  if ! systemd-run --user --scope --quiet --collect --expand-environment=no \
      --unit="$probe_unit" --property=MemoryMax=64M -- /bin/true >"$TMP/probe.log" 2>&1; then
    record FAIL "systemd.user_scope" "$(tail -n 2 "$TMP/probe.log" | tr '\n' ' ')"
    return 0
  fi
  record PASS "systemd.user_scope" "capped --user --scope works for $(id -un)"

  local ports
  ports=$(python3 "$HELPER" pick-ports) || die "cannot pick free loopback ports"
  split_ports "$ports" || die "cannot parse the loopback ports: $ports"
  local coord_url="http://127.0.0.1:$COORD_PORT"
  local device_id="verify-linux-$$"

  cat > "$STATE/config.toml" <<EOF
[coordinator]
listen = "127.0.0.1:$COORD_PORT"
url = "$coord_url"
lease_secs = 120
offline_after_secs = 90
metrics_retention_days = 1

[agent]
device_id = "$device_id"
profile = "linux_server"
listen = "127.0.0.1:$AGENT_PORT"
sample_secs = 1
heartbeat_secs = 1
ai_accounts = []
command_allowlist = ["$PYTHON_BIN"]

[advisor]
enabled = false

[guard]
max_jobs = 1
ram_budget_mib = 512
kill_after_breaches = 100
resume_after_clear = 1
mem_available_mib_pause = 0
mem_available_mib_kill = 0
swap_used_mib_pause = 1048576
swap_used_mib_kill = 1048576
swap_in_pages_per_sec_pause = 1000000.0
psi_mem_some_pause = 1000.0
psi_mem_full_kill = 1000.0
psi_io_full_pause = 1000.0
psi_io_full_kill = 1000.0
load_1m_pause = 100000.0
load_1m_kill = 100000.0
disk_free_mib_pause = 0
disk_free_mib_kill = 0
pause_on_thermal = false
EOF

  if ! run_idle init > "$TMP/init.log" 2>&1; then
    record FAIL "setup.init" "$(tail -n 2 "$TMP/init.log" | tr '\n' ' ')"
    return 0
  fi
  if [[ -f $STATE/token ]]; then
    local mode
    mode=$(stat -c %a "$STATE/token" 2>/dev/null || printf 'unknown')
    record "$([[ $mode == 600 ]] && printf PASS || printf FAIL)" "setup.token" "token mode $mode"
  else
    record FAIL "setup.token" "init did not create $STATE/token"
  fi

  COORD_UNIT="idlemax-verify-$$-coord"
  COORD_PID=$(start_scope "$COORD_UNIT" 192M 256M 20% 10 "$TMP/coord.log" \
    "$(command -v env)" -u IDLEMAX_COORD -u IDLEMAX_TOKEN "$IDLEMAX_BIN" coord)
  if wait_until 30 http_ok "$coord_url/healthz"; then
    record PASS "coordinator.start" "healthz on $coord_url"
  else
    record FAIL "coordinator.start" "$(tail -n 3 "$TMP/coord.log" | tr '\n' ' ')"
    return 0
  fi
  check_caps coordinator "$COORD_UNIT" 256 192 20000

  AGENT_UNIT="idlemax-verify-$$-agent"
  AGENT_PID=$(start_scope "$AGENT_UNIT" 576M 640M 100% 19 "$TMP/agent.log" \
    "$(command -v env)" -u IDLEMAX_COORD -u IDLEMAX_TOKEN "$IDLEMAX_BIN" agent)
  if wait_until 30 local_ready; then
    record PASS "agent.start" "local status has a metrics sample"
  else
    record FAIL "agent.start" "$(tail -n 3 "$TMP/agent.log" | tr '\n' ' ')"
    return 0
  fi
  check_caps agent "$AGENT_UNIT" 640 576 100000

  cluster_ready() {
    run_idle status --json > "$TMP/cluster.json" 2>"$TMP/status.err" \
      && python3 "$HELPER" cluster-ok "$TMP/cluster.json" "$device_id"
  }
  if wait_until 20 cluster_ready; then
    record PASS "cluster.device" "device $device_id is online with metrics"
    local out="$TMP/cluster.tsv"
    if python3 "$HELPER" cluster-check "$TMP/cluster.json" "$device_id" > "$out"; then
      local ok name detail
      while IFS=$'\t' read -r ok name detail; do
        if [[ $name != cluster.device ]]; then
          record "$ok" "$name" "$detail"
        fi
      done < "$out"
    fi
  else
    record FAIL "cluster.device" "device $device_id did not register: $(tail -n 2 "$TMP/status.err" | tr '\n' ' ')"
  fi

  check_signals
  check_memory_job
  check_cpu_job
}

main() {
  if [[ ${1:-} == --help || ${1:-} == -h ]]; then
    usage
    return 0
  fi
  if [[ ${1:-} == --self-test ]]; then
    (( $# == 1 )) || die "--self-test takes no other arguments"
    run_self_test
    return 0
  fi
  (( $# == 0 )) || die "unexpected arguments; use --help"

  [[ ${EUID:-$(id -u)} -ne 0 ]] || die "run as the normal user that will own the agent, not root"
  [[ -n ${IDLEMAX_BIN:-} ]] || die "set IDLEMAX_BIN to the idlemax binary to test"
  [[ -x $IDLEMAX_BIN ]] || die "IDLEMAX_BIN is not executable: $IDLEMAX_BIN"
  IDLEMAX_BIN=$(cd -- "$(dirname -- "$IDLEMAX_BIN")" && pwd)/$(basename -- "$IDLEMAX_BIN")
  [[ -f /proc/meminfo && -f /sys/fs/cgroup/cgroup.controllers ]] \
    || die "this is not a Linux system with cgroup v2"

  need python3
  need systemd-run
  need systemctl
  need systemd-analyze
  need awk
  need sed
  need find
  need df
  need nproc
  need mktemp

  local version_line major
  version_line=$(systemd-run --version 2>/dev/null | head -n 1) || die "cannot run systemd-run --version"
  major=${version_line#systemd }
  major=${major%% *}
  [[ $major =~ ^[0-9]+$ ]] || die "cannot parse systemd version: $version_line"
  if (( major < 254 )); then
    die "systemd $major is too old: the verification scopes need 254+ for --expand-environment=no"
  fi
  record PASS "systemd.version" "$version_line"

  TMP=$(mktemp -d "${TMPDIR:-/tmp}/idlemax-verify.XXXXXX")
  STATE="$TMP/state"
  FAKE_HOME="$TMP/home"
  mkdir -p "$STATE" "$FAKE_HOME"
  write_helper
  PYTHON_BIN=$(command -v python3)

  if [[ $TMP == /var/lib/idlemax* || $STATE == /var/lib/idlemax* || $FAKE_HOME == /var/lib/idlemax* ]]; then
    die "refusing a temp path under /var/lib/idlemax"
  fi
  if [[ -n ${HOME:-} && $FAKE_HOME == "${HOME:-}/"* ]]; then
    die "refusing to place the fake home inside the real home"
  fi

  live_checks
  verify_units
}

main "$@"
