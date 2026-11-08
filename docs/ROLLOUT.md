# Linux rollout: verify the agent, then deploy the coordinator on the tailnet

This is the runbook for ROADMAP items 1 and 2.
It assumes the server is a normal systemd host (Ubuntu 24.04 or newer), the sites on it must keep running, and idlemax may use at most one vCPU and 512 MiB of the machine.
Nothing here changes production state until the "Deploy" section.
Do the build and verification first; they use temporary paths and a temporary state directory.

The safe budget on the reference server is 1 vCPU and 512 MiB out of 2 vCPU and 3.3 GiB.
The coordinator unit caps itself at 256 MiB and 20% of one CPU.
The agent unit caps itself at 640 MiB and one CPU, which includes the one job the Linux server preset allows.

## 0. Prerequisites

- systemd 254 or newer for the verification script, which passes `--expand-environment=no` to its own scopes. Check with `systemd-run --version`.
  idlemax itself adds that flag to job scopes only on 254 or newer, so an older host still runs jobs.
- cgroup v2 mounted at `/sys/fs/cgroup` (`/sys/fs/cgroup/cgroup.controllers` exists).
- A normal login user with a systemd user manager (an SSH session is enough) to run the verification script. Do not run it as root and do not run it as the `idlemax` system user.
- Tailscale (or another private network) on the server and on every device. The coordinator binds only to the private address.
- A built Linux binary and the two unit files from `packaging/linux/`.
- `python3`, `systemd-run`, `systemctl`, `systemd-analyze`, `awk`, `sed`, `df` on the host. The script checks these and stops before doing anything if one is missing.

## 1. Get a Linux binary onto the server without starving the sites

Prefer a build on the server, inside a capped transient system unit.
The build is the only step that needs more than the agent's budget, and the cap keeps it from taking CPU or memory from the sites.

On the server, from a checkout owned by the deployment user (replace `deploy` with that user):

```sh
cd /home/deploy/idlemax
sudo systemd-run --collect --unit="idlemax-build-$(date +%s)" \
  --uid="$(id -u deploy)" --gid="$(id -g deploy)" \
  --property=CPUQuota=100% \
  --property=MemoryMax=1024M \
  --property=MemorySwapMax=0 \
  --property=IOWeight=10 \
  --property=Nice=19 \
  --property=IOSchedulingClass=idle \
  --setenv=CARGO_BUILD_JOBS=1 \
  --setenv=CARGO_INCREMENTAL=0 \
  --working-directory=/home/deploy/idlemax \
  /bin/bash -lc 'cargo build --release -p idlemax && install -m 0755 target/release/idlemax /tmp/idlemax-linux'
```

Watch it with `journalctl -u idlemax-build-<timestamp> -f`.
The unit is collected automatically when it exits.
If the build is killed at 1 GiB, raise `MemoryMax` to `1536M` for that one build, run it while the sites are quiet, and lower it again.
Never run an uncapped `cargo build` on this server.
Never build in `/var/lib/idlemax`.

The other option is to build on a Linux CI runner or a Linux container and copy only the binary:

```sh
scp target/x86_64-unknown-linux-gnu/release/idlemax deploy@server:/tmp/idlemax-linux
```

Then check it on the server:

```sh
file /tmp/idlemax-linux
ldd /tmp/idlemax-linux
sha256sum /tmp/idlemax-linux
/tmp/idlemax-linux --version
```

Compare the checksum with the build host.
Do not copy a macOS binary, a whole `target/` directory or a debug build.
A statically linked musl binary is also acceptable if it runs on the server.

Install it and create the service account:

```sh
sudo install -m 0755 /tmp/idlemax-linux /usr/local/bin/idlemax
sudo useradd --system --home-dir /var/lib/idlemax --shell /usr/sbin/nologin idlemax
sudo install -d -o idlemax -g idlemax -m 0700 /var/lib/idlemax
sudo install -o idlemax -g idlemax -m 0600 examples/config.server.toml /var/lib/idlemax/config.toml
```

Do not start the units yet.

## 2. Run the verification script

The script creates its own temporary `IDLEMAX_HOME` and `HOME`, starts a loopback coordinator and agent in capped `systemd-run --user --scope` units, and removes everything on exit.
It never reads or writes `/var/lib/idlemax` and never uses the real token or database.

Run the parser self-test anywhere first, including on the Mac:

```sh
./scripts/verify-linux.sh --self-test
```

It writes fixture files for `/proc/meminfo`, `/proc/pressure/*`, `/proc/vmstat`, `/proc/stat`, `/proc/loadavg`, `/sys/class/power_supply`, `/sys/class/thermal`, `logind`, `df` and a cgroup, then checks the parsers against them.
It needs no idlemax binary and no Linux host.

On the server, as the normal login user (not `idlemax`, not root), from the checkout:

```sh
IDLEMAX_BIN=/usr/local/bin/idlemax ./scripts/verify-linux.sh
```

If the unit files are not in `packaging/linux/` next to the checkout, set `IDLEMAX_UNITS_DIR`:

```sh
IDLEMAX_BIN=/usr/local/bin/idlemax IDLEMAX_UNITS_DIR=/home/deploy/idlemax/packaging/linux ./scripts/verify-linux.sh
```

The script checks, in order:

- systemd is 254 or newer and a capped `systemd-run --user --scope` works.
- `idlemax init` writes the token with mode 0600.
- the coordinator starts on a free loopback port inside `MemoryHigh=192M`, `MemoryMax=256M`, `MemorySwapMax=0`, `CPUQuota=20%`.
- the agent starts inside `MemoryHigh=576M`, `MemoryMax=640M`, `MemorySwapMax=0`, `CPUQuota=100%`.
- the coordinator sees the device online with a fresh metrics snapshot.
- every metrics field matches the kernel: CPU count, CPU busy percent, load, memory total and available, memory free percent, swap total and used, disk free and total (the least free of `/`, the state dir and the running jobs' working directories), logind idle, AC and battery, thermal trip state, memory pressure, PSI memory and IO, and swap-in pages per second.
- a 128 MiB memory-hog job is killed by the cgroup at its `MemoryMax` (`oom_kill` counted) and the job ends as `killed` with the error "killed by the 128 MiB memory cap".
  Job scopes set `MemoryMax` and `MemorySwapMax=0` but no `MemoryHigh`, because the kernel would throttle a hog below the cap instead of killing it.
  On systemd 253 or newer they also set `OOMPolicy=kill`, so the whole job dies with SIGKILL even when the process the kernel picked was a child of a shell; older hosts skip that property.
- a CPU-bound job with several workers stays at or below its 100% quota, throttles, and succeeds.
- `systemd-analyze verify` is clean for the coordinator and agent units.

It prints one `PASS` or `FAIL` row per check and exits non-zero if any row fails.
Set `IDLEMAX_VERIFY_KEEP=1` to keep the temporary directory and its logs when a check fails.
The temporary directory is otherwise removed on exit.

Do not enable lingering for the `idlemax` system user to make this pass.
The verification runs as your login user, which already has a user manager.
In production the agent runs as `idlemax` without lingering, so the job runner falls back to `nice` and `ionice` inside the agent unit's cgroup, which is the intended server behavior.

After installing the units, also run the exact installed files, because the script substitutes the binary path and the user in its temporary copies:

```sh
sudo systemd-analyze verify /etc/systemd/system/idlemax-coord.service /etc/systemd/system/idlemax-agent.service
```

This must print nothing and exit 0.

## 3. Deploy the coordinator on the tailnet

### 3.1 Set the listen address

Get the server's tailnet IPv4 address:

```sh
tailscale ip -4
```

Edit `/var/lib/idlemax/config.toml` and set both coordinator addresses to that address, never `0.0.0.0` or `[::]`:

```toml
[coordinator]
listen = "100.x.y.z:7878"
url = "http://100.x.y.z:7878"
```

The coordinator answers only to IP addresses, `localhost` and the host of `url` (a defence against DNS rebinding).
If a device reaches it by a MagicDNS name, list that name, or the device gets HTTP 421 (`idlemax doctor` names the fix):

```toml
[coordinator]
allowed_hosts = ["server.tail1234.ts.net"]
```

Set `[agent] device_id` to the server name used in job specs, for example:

```toml
[agent]
device_id = "my-server"
profile = "linux_server"
listen = "127.0.0.1:7879"
command_allowlist = ["gh", "git"]
```

Keep `ai_accounts = []` on the server unless that server has its own CLI logins.
Keep `[[quota.accounts]]` identical on every host.

The coordinator refuses an all-interface bind and the CLI refuses to start if the address is malformed.
A tailnet address that does not exist yet is a bind error; the unit restarts after 10 seconds until Tailscale is up.

### 3.2 Install and start the units

```sh
sudo install -m 0644 packaging/linux/idlemax-coord.service packaging/linux/idlemax-agent.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now idlemax-coord
sudo systemctl status idlemax-coord --no-pager
```

Wait for the health endpoint on the server:

```sh
curl -fsS http://100.x.y.z:7878/healthz
```

It must print `ok`.
Then start the agent:

```sh
sudo systemctl enable --now idlemax-agent
sudo systemctl status idlemax-agent --no-pager
```

From the Mac, on the tailnet:

```sh
curl -fsS http://100.x.y.z:7878/healthz
idlemax status
```

The dashboard is `http://100.x.y.z:7878/` and asks for the token once.

The coordinator unit starts after `network-online.target` and `tailscaled.service`, but it does not require `tailscaled` to be enabled.
If Tailscale is not installed there, the ordering is ignored and the bind fails until the address exists.
The restart loop is intentional.

### 3.3 Give each device the shared token

On a new device with no `~/.idlemax/token`, use `init --token-stdin`:

```sh
ssh server 'sudo cat /var/lib/idlemax/token' \
  | idlemax init --coord-url http://100.x.y.z:7878 --token-stdin
```

The token is read from stdin, not argv, and `init` writes it with mode 0600.

If the device already has `~/.idlemax/config.toml`, `init` refuses `--coord-url` because it never overwrites an existing config.
Edit `coordinator.url` in that file first, then move the old token aside and let `init` create the new one:

```sh
mv ~/.idlemax/token ~/.idlemax/token.bak
ssh server 'sudo cat /var/lib/idlemax/token' | idlemax init --token-stdin
rm ~/.idlemax/token.bak
```

If both config and token already exist, `init` keeps them.
Stop the agent before rotating a token and distribute the new token the same way.

On the Mac, point the agent at the server and start it:

```sh
# ~/.idlemax/config.toml
[coordinator]
url = "http://100.x.y.z:7878"
```

Install and start the LaunchAgent with `idlemax service install` (see "Keep it running" in the README), then check:

```sh
idlemax status
idlemax local status
```

The device must appear online with a heartbeat younger than `coordinator.offline_after_secs` (90 seconds by default).

### 3.4 Confirm the API and dashboard are not public

On the server:

```sh
sudo ss -ltnp 'sport = :7878'
```

The only listening socket for 7878 must be the tailnet address.
It must not be `0.0.0.0:7878` or `[::]:7878`.
`curl` from a host outside the tailnet, for example a phone on cellular using the server's public address, must fail or time out:

```sh
curl --max-time 5 http://<public-ip>:7878/healthz
```

Do not open 7878 in the host firewall or in a cloud firewall.
Use the tailnet ACL to limit which devices may reach 7878.
The dashboard and API share the same socket and the same bearer token.

## 4. The 24-hour check

### 4.1 Heartbeat continuity

At the start, note the time.
After 24 hours, check the device row:

```sh
idlemax status --json
```

The device must be `online` and `last_seen_ms` must be within 90 seconds of now.
Then check the per-minute history in the coordinator database:

```sh
sudo -u idlemax sqlite3 /var/lib/idlemax/coord.db \
  "SELECT count(*), min(minute_ms), max(minute_ms), sum(samples)
     FROM metrics_minute
    WHERE device = 'my-server'
      AND minute_ms >= (strftime('%s','now') * 1000 - 86400000);"
```

A healthy day has about 1440 rows, a span of about 24 hours and at least two samples per row for a 30-second heartbeat.
A gap is a pair of adjacent minutes more than 60 seconds apart:

```sh
sudo -u idlemax sqlite3 /var/lib/idlemax/coord.db \
  "SELECT a.minute_ms, b.minute_ms
     FROM metrics_minute a
     JOIN metrics_minute b
       ON b.device = a.device
      AND b.minute_ms = (SELECT min(c.minute_ms)
                           FROM metrics_minute c
                          WHERE c.device = a.device
                            AND c.minute_ms > a.minute_ms)
    WHERE a.device = 'my-server'
      AND b.minute_ms - a.minute_ms > 60000;"
```

This must print nothing.
A single missing minute is a gap even if the device is online now.
Check `idlemax events` and `journalctl -u idlemax-agent` for restarts or network errors around the gap.

The Mac side must have its agent running for the same 24 hours.
Check `idlemax local status` on the Mac and `idlemax status` on the server.
The Mac agent reconnects after sleep, so a gap while the Mac is asleep is still a gap for this acceptance check; keep the Mac awake or record the sleep as a failed day and repeat.

### 4.2 API and dashboard scope

Repeat 3.4.
The tailnet address must still be the only listener, and an off-tailnet request must still fail.

### 4.3 The coordinator stayed inside its caps

```sh
systemctl show idlemax-coord \
  -p MemoryCurrent -p MemoryPeak -p CPUUsageNSec -p NRestarts -p ActiveEnterTimestampMonotonic
```

`MemoryPeak` may not exist on older systemd; read the cgroup file instead:

```sh
sudo cat /sys/fs/cgroup/system.slice/idlemax-coord.service/memory.peak
sudo cat /sys/fs/cgroup/system.slice/idlemax-coord.service/memory.max
sudo cat /sys/fs/cgroup/system.slice/idlemax-coord.service/cpu.max
```

Requirements for the whole day:

- `memory.peak` is at or below 268435456 bytes (256 MiB).
- `memory.max` is 268435456.
- `cpu.max` is `20000 100000`.
- `NRestarts` is 0 for the day. A restart resets the average clock and is a failed day for the cap check, even if the current value looks fine.
- The average CPU used since the unit started is at or below 20%.

Compute the average from the cumulative counter and the monotonic start time:

```sh
now_us=$(awk '{ printf "%.0f\n", $1 * 1000000 }' /proc/uptime)
active_us=$(systemctl show -p ActiveEnterTimestampMonotonic --value idlemax-coord)
usage_ns=$(systemctl show -p CPUUsageNSec --value idlemax-coord)
awk -v u="$usage_ns" -v n="$now_us" -v a="$active_us" \
  'BEGIN { if (n > a) printf "coordinator average CPU: %.2f%%\n", (u / ((n - a) * 1000)) * 100; else print "no active interval" }'
```

The same check for the agent, with its own limits:

```sh
sudo cat /sys/fs/cgroup/system.slice/idlemax-agent.service/memory.peak
sudo cat /sys/fs/cgroup/system.slice/idlemax-agent.service/memory.max
sudo cat /sys/fs/cgroup/system.slice/idlemax-agent.service/cpu.max
```

The agent limits are 671088640 bytes (640 MiB), `memory.max` 671088640 and `cpu.max` `100000 100000`.
A job run during the day may push the agent's peak toward its limit, but not over it.

Also check for the kernel OOM killer and unit failures:

```sh
sudo journalctl -k --since "24 hours ago" | grep -i oom
sudo journalctl -u idlemax-coord -u idlemax-agent --since "24 hours ago" -p warning
systemctl is-active idlemax-coord idlemax-agent
```

Only the expected warnings may appear.
Any OOM kill of a site process, any failed idlemax unit, or any new error in the idlemax logs fails the day.

## 5. Rollback

Take backups before the first production start.
A short stop is acceptable for the database backup; an online SQLite backup also works:

```sh
sudo -u idlemax sqlite3 /var/lib/idlemax/coord.db \
  ".backup '/var/lib/idlemax/coord-before-rollout.db'"
sudo cp -a /var/lib/idlemax/config.toml /var/lib/idlemax/config-before-rollout.toml
sudo cp /usr/local/bin/idlemax /usr/local/bin/idlemax.before-rollout
sudo cp /etc/systemd/system/idlemax-coord.service /root/idlemax-coord.service.before-rollout
sudo cp /etc/systemd/system/idlemax-agent.service /root/idlemax-agent.service.before-rollout
```

To roll back:

```sh
sudo systemctl disable --now idlemax-agent idlemax-coord
sudo install -m 0755 /usr/local/bin/idlemax.before-rollout /usr/local/bin/idlemax
sudo install -m 0600 -o idlemax -g idlemax /var/lib/idlemax/config-before-rollout.toml /var/lib/idlemax/config.toml
sudo install -m 0600 -o idlemax -g idlemax /var/lib/idlemax/coord-before-rollout.db /var/lib/idlemax/coord.db
sudo install -m 0644 /root/idlemax-coord.service.before-rollout /etc/systemd/system/idlemax-coord.service
sudo install -m 0644 /root/idlemax-agent.service.before-rollout /etc/systemd/system/idlemax-agent.service
sudo systemctl daemon-reload
sudo systemctl start idlemax-coord
curl -fsS http://100.x.y.z:7878/healthz
sudo systemctl start idlemax-agent
```

If the rollback goes all the way to a server with no idlemax units, remove the units instead of restoring them:

```sh
sudo systemctl disable --now idlemax-agent idlemax-coord
sudo rm -f /etc/systemd/system/idlemax-coord.service /etc/systemd/system/idlemax-agent.service
sudo systemctl daemon-reload
```

If the coordinator must go back to loopback for a local-only setup, set `coordinator.listen = "127.0.0.1:7878"` and `coordinator.url = "http://127.0.0.1:7878"`, then restart the coordinator and point every agent at the new URL.
Remove the old tailnet URL from the Mac config.

If a token leaked, stop the agent, remove `/var/lib/idlemax/token`, run `sudo -u idlemax env IDLEMAX_HOME=/var/lib/idlemax idlemax init` to create a new one, and redistribute it with `--token-stdin`.
The token is a bearer credential: anyone with it can run any allowlisted command on the devices.

The verification script never touches production state.
If it fails before the deploy, there is nothing to roll back; fix the failing row and run it again.

## 6. Unit review notes

Both shipped units were reviewed against the systemd man pages.
The checks covered the caps, restart behavior, ordering, sandboxing and capability sets.

Confirmed choices, not defects:

- The coordinator uses `MemoryHigh=192M`, `MemoryMax=256M`, `MemorySwapMax=0`, `CPUQuota=20%`, `CPUWeight=10` and `IOWeight=10`. This is the 256 MiB and 20% budget from the roadmap.
- The agent uses `MemoryHigh=576M`, `MemoryMax=640M`, `MemorySwapMax=0`, `CPUQuota=100%`, `CPUWeight=10`, `IOWeight=10`, `Nice=19` and the idle I/O class. The extra 128 MiB above the 512 MiB job budget is the agent process itself.
- `Restart=on-failure` with `RestartSec=10` is what makes a missing tailnet address a retry instead of a permanent failure.
- The coordinator has an empty `CapabilityBoundingSet` and `AmbientCapabilities`, `MemoryDenyWriteExecute=yes` and `ProtectSystem=strict`; it only writes through `StateDirectory=idlemax`.
- The agent deliberately omits `MemoryDenyWriteExecute` because ordinary jobs such as Node use JIT pages.
- The agent uses `ProtectHome=read-only` and `PrivateTmp=yes`; jobs write only to `/var/lib/idlemax` and their private `/tmp`.
- `KillMode=mixed` and `TimeoutStopSec=30` let the agent stop its jobs before systemd kills the unit.
- Lingering stays off for `idlemax`. With no user manager bus, jobs run under `nice` and `ionice` inside the agent cgroup, so the unit caps bound the agent plus its one job.
- No defect was confirmed, so no unit directive was changed.

The job runner passes `--expand-environment=no` to each job scope on systemd 254 or newer, the first release with that flag.
systemd 258 made `${VAR}` expansion the default for scopes, so without it a job's arguments would be rewritten before the job saw them.
An older or unknown systemd gets no flag, so its jobs still start.
The verification script passes the same flag to its own scopes, so systemd 254 is a prerequisite for the script, not for idlemax.

## 7. Acceptance checklist

ROADMAP 1, done when:

- [ ] `signal.*` rows are PASS: every Linux metrics field matches `/proc`, `/sys`, logind and `df`.
- [ ] `job.memory.max`, `job.memory.contained`, `job.memory.peak`, `job.memory.oom` and `job.memory.killed` are PASS: the 128 MiB memory hog was killed at its cgroup limit and reported as killed by the memory cap.
- [ ] `job.cpu.quota`, `job.cpu.percent`, `job.cpu.throttled` and `job.cpu.completed` are PASS: the CPU job stayed within 100% of one CPU.
- [ ] `units.verify` is PASS and `sudo systemd-analyze verify /etc/systemd/system/idlemax-coord.service /etc/systemd/system/idlemax-agent.service` prints nothing.

ROADMAP 2, done when:

- [ ] `coordinator.listen` is the tailnet address and `ss` shows only that address on 7878.
- [ ] Each device got the token with `idlemax init --token-stdin` and the Mac agent's `coordinator.url` points at the server.
- [ ] The Mac agent heartbeats for 24 hours with no missing `metrics_minute` row and the device is online.
- [ ] An off-tailnet request to 7878 fails, and the dashboard is reachable only on the tailnet.
- [ ] `memory.peak <= 268435456`, `cpu.max = 20000 100000`, `NRestarts = 0`, and the average coordinator CPU is at or below 20% for the day.

If any box is unchecked, the rollout is not done.
Record the failing row or SQL result and keep the old coordinator running until it is fixed.
