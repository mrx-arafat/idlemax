# 6. Quota and the Budget ⏳

**What you will learn:** how idlemax reads your AI quota, how to add your accounts, the reserve that protects you, how the daily Budget for workers is worked out, the measuring week, model tiers, what happens at a usage limit, and every `quota.*` setting.

## Why bother?

A Claude or Codex subscription gives you quota in windows that reset.
A typical week: you use 55% of your weekly Claude quota, the window resets, and the other 45% is simply gone.
idlemax spends what would expire anyway, and keeps your share untouched.

## Read this first: the ban-risk stance

idlemax only launches the unmodified official CLIs (`claude -p`, `codex exec`, `agy -p`) with your own login, on the device where you logged in.
It never copies credentials between devices, never fans out across devices or accounts, never retries an AI job, and stops spending an account for the rest of the window after any limit message.

That keeps automated use close to ordinary individual use.
But consumer terms still restrict automated access, so **the risk is reduced, not zero**.
Metered API keys (`provider = "api"`) are the path providers designate for automation.

> [!CAUTION]
> In a test or dev setup, never configure an AI account or queue an `ai` job: it launches the real CLI and spends real quota.

Before you add any account, `idlemax quota` simply says so (exit 0):

```text
no AI accounts configured ([[quota.accounts]] in config.toml)
```

## Step 1: tell idlemax about your accounts

Accounts live in `config.toml` as `[[quota.accounts]]` tables.
This is a hand edit (`idlemax config set` cannot write a list of tables).
Open `~/.idlemax/config.toml` in any text editor and add:

```toml
[[quota.accounts]]
name = "claude-max"
provider = "claude"
probe = "claude_statusline"
windows = [
  { kind = "rolling", minutes = 300, reserve_pct = 40.0 },
  { kind = "weekly", reserve_pct = 30.0, final_day_reserve_pct = 15.0 },
]

[[quota.accounts]]
name = "codex-pro"
provider = "codex"
probe = "codex_sessions"
windows = [{ kind = "weekly", reserve_pct = 30.0, final_day_reserve_pct = 15.0 }]
```

Then tell each device which accounts it may spend, only where you logged in to that CLI yourself:

```sh
idlemax config set agent.ai_accounts claude-max,codex-pro
```

Both changes are read only when the agent starts (with the agent running, `config set` says `saved; restart the agent to apply it, it reads this only at start`), so restart it:

```sh
launchctl kickstart -k gui/$(id -u)/com.idlemax.agent   # macOS
systemctl --user restart idlemax-agent                  # Linux, user unit
```

If you run the agent in a terminal instead, stop it with `Ctrl-C` and start `idlemax agent` again.

| Field | Meaning |
|---|---|
| `name` | your name for the account; use the same name on every device for the same login |
| `provider` | `claude`, `codex`, `agy`, `opencode` or `api` |
| `windows` | optional; left out, the provider defaults below apply |
| `probe` | where usage is read: `claude_statusline`, `codex_sessions` or `manual` |
| `probe_path` | optional; a different file or folder for the probe |
| `daily_cap_runs` | `agy` and `opencode` only: worker Runs per UTC day |

> [!IMPORTANT]
> Keep `[[quota.accounts]]` identical on every device and on the coordinator.

### Provider defaults

| Provider | Default windows | Usage source |
|---|---|---|
| `claude` | 5-hour rolling (reserve 40%), weekly (reserve 30%) | Claude Code statusline capture |
| `codex` | weekly (reserve 30%), plus a 5-hour window if Codex reports one | newest Codex session file |
| `agy` | 5-hour rolling (40%), weekly (30%) | manual file |
| `opencode` | none | manual file |
| `api` | none | manual file |

The weekly reserve drops to 15% in the last 24 hours before the reset.

## Step 2: let idlemax see your usage

### Claude: the statusline capture

Claude usage comes from the JSON that Claude Code passes to its statusline command.
idlemax wraps your `statusLine` command so each render also saves that JSON to `$IDLEMAX_HOME/agent/claude-statusline.json`.

```sh
idlemax statusline install --dry-run   # shows the diff, writes nothing
idlemax statusline install
idlemax statusline status              # is it wired, and when did the last capture land
idlemax statusline uninstall           # puts the original command back
```

Then open Claude Code once so it renders the statusline.
Until that first capture lands, Claude accounts are **never** spendable (fail closed), and `statusline status` says "not written yet; open Claude Code so it renders the statusline".

What `install` does to your `settings.json` (in `$CLAUDE_CONFIG_DIR`, else `~/.claude`), and nothing else:

- copies the file to `settings.json.idlemax.bak` before every change;
- changes only the `statusLine.command` string; every other key, the key order, the indentation and the line endings stay byte for byte;
- runs your original command unchanged under `sh -c`, so the statusline you see is identical, and ignores capture errors, so the statusline never breaks;
- with no `statusLine` yet, adds one that runs the capture script alone, and with no `settings.json` at all, creates one (`statusline status` then says "no settings.json yet; `idlemax statusline install` creates one");
- writes the capture script to `$IDLEMAX_HOME/bin/claude-statusline-capture.sh`, and puts `IDLEMAX_HOME` into the command when it is set.

Running `install` again changes nothing.
It refuses, without touching the file, when `settings.json` is not valid JSON, when `statusLine` is not an object with `"type": "command"` and a `command` string, or when `statusLine` appears twice.

> [!TIP]
> `idlemax setup --statusline` does the install as part of setup.

### Codex: session files

`probe = "codex_sessions"` reads the newest Codex session files under `$CODEX_HOME/sessions` (default `~/.codex/sessions`).
Nothing to install.

### agy, OpenCode and API: a manual file

`probe = "manual"` reads `$IDLEMAX_HOME/agent/quota-<account>.json`, a list you write yourself:

```json
[{"kind": "weekly", "used_pct": 40, "resets_at_ms": 1790000000000}]
```

`agy` and `opencode` accounts have no usage reading for workers, so workers run on `daily_cap_runs`, a fixed number of Runs per UTC day.
Without it, no worker starts on them:

```toml
[[quota.accounts]]
name = "agy-main"
provider = "agy"
probe = "manual"
daily_cap_runs = 5
```

## Step 3: check it

```sh
idlemax quota
```

```text
ACCOUNT     WINDOW  USED  RESERVE  HEADROOM  RESETS     SPEND  WHY
claude-max  5h      34%   40%      26%       in 1h19m   no     another AI job holds this account's lease; an AI job alre...
claude-max  week    41%   30%      29%       in 3d4h    no     another AI job holds this account's lease; an AI job alre...
codex-pro   week    31%   15%      54%       in 18h59m  yes    weekly window in its final third, resets in 18h 59m

ACCOUNT     BUDGET  SPENT   LEFT    WORKERS  DAY ENDS   WHY
claude-max  8.2%    2.0%    6.2%    yes      in 4h00m   41% used, the owner's usual use holds 18%, margin 10%: 31% surplus over 4 days
codex-pro   54.0%   0.0%    54.0%   yes      in 18h59m  last day before the reset: 31% used, the floor is 15%
```

(Demo data.)
Every account in `[[quota.accounts]]` gets a row in both tables.
An `agy` or `opencode` account counts its Budget in Runs, for example `agy-main  5 runs  3 runs  2 runs  yes  in 9h02m  daily cap: 3 of 5 runs used today`.
The first table is per window: used, your reserve, the headroom above it, and whether an old-style AI job may spend it now (`SPEND`, `WHY`).
The second table is the **Budget** for dispatcher workers today: `SPENT` is what idlemax's Runs that ended today cost, `LEFT` what is still free after the running ones, and `WORKERS` whether a worker may start now.

Any account problem (see [one login, one name](#one-login-one-name)) is printed under the tables with its fix.

## The Budget, explained with numbers

A worker's Budget is the account's **surplus**: what of the weekly window would expire unused at the reset, given your usual daily use, minus a safety margin, spread over the account days left.

An **account day** is a 24-hour slot counted back from the weekly reset.

Worked example (the `claude-max` row above):

1. The weekly window is 41% used and resets in 3 days 4 hours, so 4 account days are left; today's ends in 4 hours.
2. Your usual use over those 4 days is learned to be 18%.
3. The safety margin is 10% (`quota.safety_margin_pct`).
4. Surplus: 100 - 41 - 18 - 10 = 31% of the week.
5. Today idlemax already spent 2%. It adds that back and spreads the total over the days left: (31 + 2) / 4 = 8.25%, shown as `8.2%`.
   Adding today's spend back keeps the day's figure steady while it is spent.
6. `LEFT` is 8.25 - 2 = 6.25%, shown as `6.2%`.

On the last account day the rule changes: idlemax may spend what is left down to the floor (the `codex-pro` row: 100 - 31 - 15 = 54%).

Your usual use is learned from the last 7 account days.
Until idlemax has a week of your numbers, it reserves `quota.owner_reserve_pct` (50%) of the weekly quota for you.

### The guard rails

| Rule | Setting | Default |
|---|---|---|
| The **5-hour stop**: at this share of the 5-hour window no worker starts, and running workers stop with their state saved until the window allows | `quota.five_hour_stop_pct` | 60 |
| The **last day**: in the last 24 hours before the reset (the final account day), idlemax may spend what is left down to this floor | `quota.last_day_floor_pct` | 15 |
| The **measuring week**: in an account's first week of Runs, idlemax spends at most this share of each weekly window | `quota.measuring_week_cap_pct` | 10 |
| A **server-only** account (only servers list it) holds only this much for you, and its surplus is paced evenly over the days to the reset | `quota.server_only_margin_pct` | 5 |

When an account's Budget for today is spent, no new worker starts on it, but running workers finish.
Work moves to another account, or waits for tomorrow.

### The measuring week

Every Run records provider, account, model, task type and the quota before and after, on the 5-hour and weekly windows.
That builds a real cost table per model.
Before a model has measurements, its cost is estimated from `quota.seed_small_pct` (3% of the week) and `quota.seed_standard_pct` (8%).
A Run starts only when its estimated cost fits in what is left of today's Budget.

## Model tiers

| Tier | For | Claude model | Setting |
|---|---|---|---|
| `small` | task types in `quota.small_task_types` (default `broken_check`) | `haiku` | `quota.claude_small_model` |
| `standard` | everything else, and the second try after one failed check | `sonnet` | `quota.claude_standard_model` |

A task that fails its checks twice does not get a third try: it goes on "needs you".
The orchestrator itself always uses Opus, and Opus is refused as a worker model: a Claude tier model that contains `opus` is rejected by `idlemax config set` and at startup.
Other providers' models ship unset (`config list` shows `off`) (`quota.codex_small_model`, `quota.codex_standard_model`, `quota.agy_*`, `quota.opencode_*`), and a provider whose tier model is not set gets no worker for that tier:

```sh
idlemax config set quota.codex_standard_model <model>
```

## When a limit is hit

On a usage-limit message or HTTP 429, idlemax:

- stops that account's workers with their state saved;
- **rests** the account until the used-up window resets, that is the window that reads 90% or more (basis `resting` in the plan); this is the weekly reset only when the weekly window is the one used up;
  when no window reads as used up, it rests until the account's shortest window resets, or 5 hours after the hit for a 5-hour window with no reading yet;
  an `agy` or `opencode` account (one with `daily_cap_runs`) rests until the end of the current UTC day instead;
- moves waiting tasks to another account;
- never retries in a loop.

A second limit message while the account rests changes nothing.
Only a real limit counts: in the JSON result of `claude -p`, a line counts only when it is an error (`is_error`, an `error` field or `"type": "error"`) whose text says so, never because a number in it reads 429.
idlemax's own dispatcher jobs (the orchestrator pass, a check) never rest an account.
Old-style AI jobs are blocked on that account until the window that saw the limit resets.

## Old AI jobs vs dispatcher workers

An "old" AI job is a job with `cost = "ai"` you queue yourself (such as the PR review drafts) or the advisor's pass.
The two follow different rules:

| Rule | Old AI job | Worker |
|---|---|---|
| The device lists the account in `agent.ai_accounts` | applies | applies |
| `idlemax pause --account` | applies | applies; a running worker freezes in place |
| Usage data is trusted and under 6 hours old | applies | applies to a start |
| A usage limit or HTTP 429 was seen | blocked until that window resets | rests until the used-up window resets; running workers stop |
| Reserve of 40% per 5-hour window, 30% weekly, 15% in the final day | applies | replaced by the Budget, the 5-hour stop and the last-day floor |
| Only in the final third of the shortest window, or within 24 h of the weekly reset | applies | does not apply: the Budget paces by account day |
| Hold for 30 minutes after interactive use | applies | does not apply; accounts you are not using are preferred instead |
| One AI lease per account | applies among old AI jobs | does not apply |
| One AI job per account per 5 hours | applies among old AI jobs | does not apply |
| Headroom for the job's `token_class` | the fixed figures 3, 8 and 20% | replaced by: the Run's estimated cost fits in today's Budget |

The dispatcher's orchestrator pass is an old AI job that skips the headroom row.

## One login, one name

One account name means one login on every device, and one login has one name.
Two devices on the same login share one Budget; different logins never do.

The coordinator checks this from the logins the agents report (`claude auth status --json`; no token is read) and closes what breaks it for workers:

- A Claude account is closed on a device that lists it and reports a logged-out login, or no login it can read (`cannot tell whether`: run `idlemax doctor` there).
- An account name with two different logins is closed everywhere: give one login its own `[[quota.accounts]]` entry.
- When two names resolve to one login, every name after the first is closed: use one name on every device.
- For Codex, the name you give in `agent.ai_accounts` is the account; list the same name on every device that uses that login.
  Two Codex names whose weekly readings reset within 5 minutes and read within 1 point of each other raise a warning that they may be one login.
- A laptop that has `claude` but reports no login keeps the full reserve and raises an `OwnerMayUse` warning; add a `[[quota.accounts]]` entry for the laptop's own Claude account so its agent reports the login.

## Every quota setting

| Key | Default | What it does | When to change it |
|---|---|---|---|
| `quota.owner_reserve_pct` | 50.0 | weekly percent reserved for you until idlemax has learned your use | raise it if you are a heavy user in a new setup |
| `quota.safety_margin_pct` | 10.0 | margin subtracted from the surplus | raise it if you ever ran short |
| `quota.five_hour_stop_pct` | 60.0 | 5-hour usage at which workers stop | lower it to keep more of each 5-hour window for you |
| `quota.last_day_floor_pct` | 15.0 | weekly floor in the last 24 hours before the reset | lower it to spend more of a quiet week |
| `quota.server_only_margin_pct` | 5.0 | weekly percent held for you on an account only servers use | raise it if you sometimes use that login yourself |
| `quota.measuring_week_cap_pct` | 10.0 | weekly percent idlemax may spend in an account's first week (above 0, at most 100) | keep it low until you trust the numbers |
| `quota.seed_small_pct` | 3.0 | estimated cost of an unmeasured small Run (weekly %) | rarely |
| `quota.seed_standard_pct` | 8.0 | estimated cost of an unmeasured standard Run (weekly %) | rarely |
| `quota.small_task_types` | `["broken_check"]` | task sources that start on the small tier | add `roadmap` if your roadmap items are tiny |
| `quota.claude_small_model` | `haiku` | Claude small tier model; a name containing `opus` is refused | |
| `quota.claude_standard_model` | `sonnet` | Claude standard tier model; a name containing `opus` is refused | |
| `quota.codex_small_model` | off (unset) | Codex small tier model | to let Codex take small tasks |
| `quota.codex_standard_model` | off (unset) | Codex standard tier model | to let Codex take standard tasks |
| `quota.agy_small_model` | off (unset) | agy small tier model | |
| `quota.agy_standard_model` | off (unset) | agy standard tier model | |
| `quota.opencode_small_model` | off (unset) | OpenCode small tier model | |
| `quota.opencode_standard_model` | off (unset) | OpenCode standard tier model | |
| `[[quota.accounts]]` | none | your accounts (hand edit) | |

Every `quota.*` setting except `quota.accounts` applies at once, with no restart.

## The advisor's AI pass

The advisor (see [chapter 7](07-dashboard-tui-mcp.md#the-advisor)) can also spend an account on its own pass, through `advisor.ai_account`.
The orchestrator pass counts under the rule of one AI job per account per 5 hours, so set `advisor.ai_account` to a different account than the one the dispatcher uses.
`advisor.*` settings are read when the coordinator starts, so restart it after a change (`launchctl kickstart -k gui/$(id -u)/com.idlemax.coord` on macOS, `systemctl --user restart idlemax-coord` on Linux, or the system unit on a server).

Next: [Dashboard, TUI and MCP](07-dashboard-tui-mcp.md)
