# Dispatcher 3: The Quota Brain Implementation Plan

Created: 2026-11-11
Agent: Claude Code
Status: COMPLETE
Approved: Yes
Iterations: 1
Worktree: No
Type: Feature

## Summary

**Goal:** idlemax spends only the quota the owner would leave unused, paced by account day, on the cheapest model that passes the checks, and `idlemax quota` shows each account's budget for today with the reason for its number.

## Out of Scope

- Storing the Run, and the start, the end and the 10 percent cap of the measuring week: Spec 2 owns them (A7), and this plan reads them.
- Reading the text that ended a worker's turn: Spec 2's runner seam reports it as `end_text` (A9), and this plan only classifies it.
- Taking worker jobs out of the one-AI-lease rule and the one-start-per-5-hours rule: Spec 2 does that in both directions (A8).
- The lean worker launch and the short orchestrator pass of D10.9: Spec 2 and Spec 1 build them, and this plan builds the quota side only (spec map).
- Dashboard and TUI views of the budget, and notifications: Spec 4 reads the `budget` object this plan adds to `GET /v1/quota` and the stored account days.
- Account identity across devices and the server-only account rule: Spec 5 fills the input `owner_reserve_override_pct` that Task 5 leaves for it.
- A quota reader for agy or OpenCode: they get a fixed daily cap (D10.8), and no worker starts on them until Spec 2 lists their CLI in `workers.clis` (A21).
- Windows per model: the probes read one 5-hour and one weekly figure per account, and Task 1 only records whether more exist.
- The rules and the fixed cost figures of old AI jobs (a `cost = ai` job with no `worker` section): they stay exactly as they are (A8), and that covers `limit_block`, `job_run::rate_limited` and `token_class_pct`.
- Counting the spend of the orchestrator pass, of Spec 2's reviewer runs and of old AI jobs as idlemax's own: none of them is a Run, so their use lands on the owner's side of an account day, which makes the surplus smaller and never larger.

## Approach

**Chosen:** New pure modules under `crates/idlemax-core/src/quota/` (`config`, `tier`, `cost`, `owner_use`, `surplus`, `budget`, `spend`, `pass`) next to the existing `rules.rs`, one thin coordinator adapter (`coord/api/budget.rs` over `coord/store/budget.rs`), and the Budget attached to the rows of the existing `GET /v1/quota`.
**Why:** Every number is a pure function of stored readings and Run records with `now_ms` as a parameter, so it is unit tested without a clock, and the old AI job verdict in `quota/rules.rs` is not touched; the cost is one new table and one more gate that every worker start must pass.

## Context for Implementer

Quota is read as a used percent per account on two windows, the 5-hour one and the weekly one, never per model and never in tokens.
All Budget percents are percent of the weekly window, except for accounts on a daily cap, where they are percent of the day's cap; the two units are never compared.
An account day is a 24-hour slot counted back from the weekly reset (A25), so "days until reset" and "the day before reset" need no time zone.
Every rule here fails closed: no trusted reading, an unset model name or an unset cap means no worker starts on the account, with a reason that names the fix.
The Budget is one more gate and never a bypass: a worker starts only when the lines of D14, every pause, the kill switch `$IDLEMAX_HOME/PAUSE` and the gate of Task 5 all allow it.
No test and no manual check in this plan configures a real AI account or starts a real `claude`, `codex`, `agy` or `opencode`; tests use `Store::open_in_memory` (`crates/idlemax/src/coord/store.rs:150-153`) or a temp home built with the recipe in `AGENTS.md`.

The tasks use these names of Specs 1 and 2, with the paths those specs give:

| Name | Where | Used in |
|---|---|---|
| `account_uses(cfg, views, paused, spent_week_pct)`, `AccountUse`, `MEASURING_WEEK_CAP_PCT` (the simple rule) | `crates/idlemax-core/src/dispatch/accounts.rs` (Spec 1) | Tasks 6, 9, 10 |
| `gather_plan(store, cfg, live, now)`, `step` | `crates/idlemax/src/dispatch/mod.rs` (Spec 1) | Task 10 |
| `LiveConfig::current()`, `AppState::live()` (the config re-read on an mtime change, A2) | `crates/idlemax/src/live_config.rs`, `crates/idlemax/src/coord/api.rs` (Spec 1) | Tasks 6, 9, 10, 11 |
| `summary`, `prompt` (the pass input) | `crates/idlemax-core/src/dispatch/prompt.rs` (Spec 1) | Task 10 |
| `validate`, `pass_due`, `DueInput`, `Offer`, `PASS_MODEL` | `crates/idlemax-core/src/dispatch/orchestrator.rs` (Spec 1) | Tasks 2, 10 |
| `build`, `PlanInput`, `PassResult`, `PassPick` | `crates/idlemax-core/src/dispatch/plan.rs` (Spec 1) | Task 10 |
| `TaskSource` | `crates/idlemax-core/src/dispatch/task.rs` (Spec 1) | Task 2 |
| `Store::judge_task(task, digest, reason, now)`, `Store::judgments` (the "needs you" store) | `crates/idlemax/src/coord/store/dispatch.rs` (Spec 1) | Task 10 |
| `DispatcherConfig.worker_models` | `crates/idlemax-core/src/config/dispatcher.rs` (Spec 1) | Task 10 |
| The plan route, `ingest_pass` and their tests | `crates/idlemax/src/coord/api/plan.rs` (Spec 1) | Task 10 |
| `Run`, `run::after_job(run, job, update, now_ms)` | `crates/idlemax-core/src/worker/run.rs` (Spec 2) | Tasks 3, 9 |
| `JobUpdate.worker` (`WorkerReport`) with `end_text` | `crates/idlemax-core/src/worker/report.rs`, `crates/idlemax-core/src/protocol.rs` (Spec 2) | Task 9 |
| The `runs` table, `runs_of_account(account, since_ms)`, the `jobs.worker` column | `crates/idlemax/src/coord/store/runs.rs`, Spec 2's migration in `coord/store/schema.rs` | Tasks 3, 4, 6 |
| `pace::week_spend`, `pace::measuring_week_end`, `pace::may_launch`, `pace::task_open` | `crates/idlemax-core/src/worker/pace.rs` (Spec 2) | Tasks 6, 10 |
| `dispatch::launch_due`, `dispatch::stop_workers` | `crates/idlemax/src/coord/dispatch.rs` (Spec 2) | Tasks 6, 9 |
| The coordinator tests of worker jobs | `crates/idlemax/src/coord/api/worker_tests.rs` (Spec 2) | Task 9 |
| `agent_args` (the worker launch form) | `crates/idlemax-core/src/worker/launch.rs` (Spec 2) | Task 1 |
| `Paths::worktrees_dir()` | `crates/idlemax-core/src/paths.rs` (Spec 2) | Task 7 |
| `workers.clis`, `workers.reviewers` | `WorkersConfig`, the `[workers]` section of Spec 2 | Tasks 6, 8 |

## File Structure

- `docs/QUOTA-COSTS.md` (create) - the measuring procedure, its results and the tuned numbers.
- `crates/idlemax-core/src/quota/config.rs` (create, pure) - `QuotaConfig` with the new `[quota]` settings, its defaults, `validate` and `QuotaConfigError`, moved out of `quota.rs`.
- `crates/idlemax-core/src/quota/tier.rs` (create, pure) - `Tier`, `tier_for`, `model_for`, `model_key`, `tier_models`, `tier_of`.
- `crates/idlemax-core/src/quota/cost.rs` (create, pure) - `RunSample`, the split of a reading's rise among overlapping Runs, `CostTable`, `expected`, `tier_costs`.
- `crates/idlemax-core/src/quota/owner_use.rs` (create, pure) - account day slots, `AccountDay`, `OwnerUse`.
- `crates/idlemax-core/src/quota/surplus.rs` (create, pure) - the owner's hold and the surplus.
- `crates/idlemax-core/src/quota/budget.rs` (create, pure) - `Budget`, `BudgetInput`, `budget`.
- `crates/idlemax-core/src/quota/spend.rs` (create, pure) - the worker rules in one place: `gate`, `stops_workers`, `offers`, `limit_rest_until`, `rest_event`.
- `crates/idlemax-core/src/quota/pass.rs` (create, pure) - what the orchestrator pass may pick: `account_offers`, `any_room`, `quota_summary`, `check_picks`.
- `crates/idlemax-core/src/quota.rs` (modify) - module lines and re-exports, `Provider::Opencode`, `daily_cap_runs`, `QuotaView.budget`, `SAME_RESET_MS`.
- `crates/idlemax/src/coord/store/budget.rs` (create) - reads Run rows as samples, reads weekly snapshots, reads and writes `quota_days`.
- `crates/idlemax/src/coord/api/budget.rs` (create) - gathers the inputs and calls the pure functions: `budgets`, `worker_gate`, `close_quota_days`, `record_open_days`, `new_rest`.
- `crates/idlemax/src/agent/quota_probe/own_sessions.rs` (create) - tells an idlemax worker's session file from the owner's.
- `crates/idlemax/src/cli/out/budget.rs` (create) - the budget table cells and its TOON list.

## Assumptions

- Specs 1 and 2 have landed (the build order of D21) with the names in the table above, and with these Answers built as those specs describe them: `LiveConfig` re-reads `config.toml` when its mtime changes (A2); a worker job keeps `cost = ai` and `ai_account`, is marked in `jobs.worker`, and is outside the one-AI-lease rule and the one-start-per-5-hours rule in both directions (A8); the measuring week starts at an account's first Run and ends seven days later (A7); the last update of a launch that added no commit carries `end_text` (A9); every pick comes from an orchestrator pass and a pass may return more picks than there are open slots (A19); the pass runs when its inputs changed, at most once an hour by itself (A20).
  Tasks 3, 6, 9 and 10 depend on this.
- A Run row of Spec 2 carries `launches` (the number of jobs started for it), the session id, the task source as its task type (A24) and `quota_before` as time-stamped snapshots.
  Tasks 3, 4 and 6 depend on this.
- The owner steps of Spec 1 are done before Tasks 1 and 12: the current build runs on the Mac, the statusline capture is installed (roadmap item 4) and the Claude and Codex accounts are configured.
  Tasks 1 and 12 depend on this.
- A Run whose verdict is `rejected` is what D10.6 calls "failed its checks".
  Tasks 8 and 10 depend on this.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| A reading does not refresh after a worker run, so the run looks free and the day overspends | Medium | High | A cost counts only with a reading taken at or after the Run's end, read from `quota_snapshots`, and a Run without one is charged at its estimate, never at zero (Task 3 tests); Task 1 items 5 and 10 record how each launch form refreshes the reading |
| Readings move in whole points, so a small run reads as 0 or 1 | Medium | Medium | The table uses the mean of up to 20 samples and keeps the seed until a key has 5 (Task 3 test) |
| The owner works on the account while workers run, so measured costs include the owner's use | High | Low | The error makes idlemax spend less, never more; the cost split and the owner's side are defined so (Tasks 3 and 4) |
| The seed of 8.0 percent for a standard Run lets very few Runs start before costs are measured | High | Medium | The seeds are settings, and the first step of Task 12 sets them from the points per run that Task 1 measured |
| A worker's own text matches a limit phrase and the whole account is stopped | Medium | Medium | Only `end_text`, the text that ended a turn with no new commit, is classified (A9, Task 9 test); a false stop costs one rest with state saved |
| A limit per model is invisible in the two readings, so an account can hit a limit again after each 5-hour rest | Medium | Low | Each hit costs one stop with state saved and no retry; Task 1 item 2 records whether a window per model exists, and Task 12 counts the `quota` events of the week |
| The migration number collides with Spec 1 or Spec 2 | High | Low | Task 4 takes the next free `Vn` after reading `MIGRATIONS` on `main` |

## Goal Verification

### Truths

1. On an account with seven known account days, no Run starts while the account's `left_pct` is below the Run's estimated cost, so a day's spend passes that day's `spendable_pct` by at most the cost of the Runs already running.
2. One usage-limit message leads to exactly one stop of that account's workers and to no new Run on that account before `rests_until_ms`, while waiting tasks are placed on other accounts.
3. An old AI job is leased under exactly the rules of today: no assertion of the existing test in `crates/idlemax-core/src/quota/rules/tests.rs` changes in any task, and `GET /v1/quota` still shows the old verdict in `spendable` and `reasons` next to the new `budget`.

## Progress Tracking

- [ ] Task 1: Needs the owner's OK: measure how each plan counts usage
- [x] Task 2: Add the quota settings, the Tier and the OpenCode provider
- [x] Task 3: Build the cost table from Run records
- [x] Task 4: Learn the owner's own use per account day
- [x] Task 5: Compute the surplus, the daily Budget and the worker rules
- [x] Task 6: Gather the Budget on the coordinator and gate every worker start
- [x] Task 7: Leave idlemax's own worker sessions out of the owner's use
- [x] Task 8: Offer accounts and check picks in pure code
- [x] Task 9: Stop an account's workers at the 5-hour line and after a rate limit
- [x] Task 10: Hold Spec 1's plan and orchestrator pass to the Budget and the tier
- [x] Task 11: Show the Budget in `idlemax quota` and `GET /v1/quota`
- [ ] Task 12: Needs the owner's OK: run the measuring week and tune the numbers

> Source of truth for completion: `spec-implement` toggles `[ ]` to `[x]` here.

## Implementation Tasks

### Task 1: Needs the owner's OK: measure how each plan counts usage

**Objective:** On the owner's real Claude and Codex accounts, run one small fixed task a few times per model and write down how far the 5-hour and the weekly reading move.
The result gives the seed costs, the model names of each tier and the rules for reading a cost, which no document states.
It spends real quota, so it runs only after the owner says yes, and it stops at a stated ceiling.

**Files:**

- Create: `docs/QUOTA-COSTS.md`

**Key Decisions / Notes:**

- This is not a dev run: this task starts no idlemax build and queues nothing, and the CLIs are run by hand by the owner or with the owner watching (D10, last paragraph).
- Ceiling per account: stop at the first of these: the weekly reading has moved 3 points from its start, the 5-hour reading has reached 60 percent (D10.4), or 10 runs were made on one model.
  These runs are not Run records, so the measuring week's count of Spec 2 does not see them (A7); the file states the points they used, so the owner sees the total.
- The fixed task is a throwaway git repository in a temp folder with one failing unit test and a one-line fix, and the same prompt text for every model, both written into the file.
  It is never one of the owner's repositories.
- The launch form is the worker form of Spec 2, `agent_args` in `crates/idlemax-core/src/worker/launch.rs`: for Claude `--model <model> --strict-mcp-config --permission-mode bypassPermissions --session-id <uuid>`, for Codex `--model <model> --sandbox workspace-write --ask-for-approval never --add-dir <git common dir>` with one `-c mcp_servers.<name>.enabled=false` for each configured server.
  Each model is run in an interactive pane with those arguments, and once headless with the same arguments (`claude -p` with the prompt on standard input, `codex exec`).
  That form runs with permissions bypassed, so it is used only inside the throwaway repository (Open Question 2).
- The models are `haiku` and `sonnet` for Claude (A21), and for Codex the names the owner wants proven, since the Codex keys ship unset until a measured run proves a name (A21).
- Readings are read-only: for Claude the fields `rate_limits.five_hour.used_percentage` and `rate_limits.seven_day.used_percentage` of the capture file (`crates/idlemax/src/agent/quota_probe.rs:174-209`), for Codex the newest `token_count` event's `rate_limits` (`quota_probe.rs:236-340`).
- A headless `claude -p` run does not refresh the capture (`crates/idlemax-core/src/quota/rules.rs:15-17`), so after a headless run the reading is refreshed by one short message in an interactive Claude Code session.
  The cost of that message alone is measured first, with two in a row, and taken off every later figure.
- The file records, per account:
  1. The plan label from `claude auth status --json` (`subscriptionType`, `planTier`; it shows no token), and for Codex the plan name the owner states.
  2. The name of every key under `rate_limits` in the capture, which shows whether a window per model exists.
  3. Whether a reading is a whole number or has decimals.
  4. Per model: the `--model` value the CLI accepted, the runs made, the reading before and after on each window, and the points per run.
  5. Whether a headless run and a run in an interactive pane each refresh the capture by themselves, judged by the capture file's modified time with no refresh message in between.
  6. For Codex: whether `codex exec` writes a session file with a `rate_limits` event, and which windows it reports.
  7. One run of the standard model with the owner's personal MCP servers loaded, against the lean run: the difference is what D10.9 saves.
  8. For agy and OpenCode, for which no cost can be read: whether `--model` is accepted, and where the CLI keeps its session files.
  9. Where each CLI's session file records the working folder (Claude: the `cwd` field, `crates/idlemax/src/advisor/sessions.rs:71-79`), which Task 7 needs.
  10. For a pane run: the capture file's modified time against the time the pane's turn ended, which shows whether the last statusline write comes after the last API call; the rule for the after reading in Task 3 rests on it.

**Definition of Done:**

- [ ] `docs/QUOTA-COSTS.md` holds the procedure, the prompt, and one filled row per account and model with the points per run on the 5-hour and the weekly window.
- [ ] For each account the file states the weekly reading at the start and at the end, and the difference is 3 points or less.
- [ ] Items 2, 3, 5, 6, 8, 9 and 10 are answered with what was seen, or with "not seen" where a thing could not be observed.
- [ ] Nothing under the idlemax home was written by this task except the capture file `agent/claude-statusline.json`, which the installed statusline wrapper rewrites on every refresh message, and no idlemax daemon was started for this task.
- [ ] Verify: `git status --short` lists only `docs/QUOTA-COSTS.md`; the task adds no code and no test, so `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass as before.

### Task 2: Add the quota settings, the Tier and the OpenCode provider

**Objective:** Put the numbers of D10 into `config.toml` as settings with the decided defaults, add the `Tier` type with its move-up rule and its model mapping, and add OpenCode as a provider with a daily cap field.
After this task the configuration can say everything the later tasks read, and nothing spends differently yet.

**Files:**

- Create: `crates/idlemax-core/src/quota/config.rs`
- Create: `crates/idlemax-core/src/quota/tier.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax-core/src/quota/rules/tests.rs`
- Modify: `crates/idlemax-core/src/config/reload.rs`
- Modify: `crates/idlemax/src/agent/metrics/caps.rs`
- Modify: `examples/config.toml`
- Modify: `README.md`
- Modify: `docs/API.md`
- Test: `crates/idlemax-core/src/quota/config.rs` and `crates/idlemax-core/src/quota/tier.rs` (inline `#[cfg(test)]` modules)
- Test: `crates/idlemax/tests/examples.rs`

**Key Decisions / Notes:**

- `QuotaConfig`, `QuotaConfig::validate` and `QuotaConfigError` (`crates/idlemax-core/src/quota.rs:20-67`) move unchanged to the new `quota/config.rs` and are re-exported from `quota.rs`, because `quota.rs` has 294 lines and this plan would take it past the size rule.
  `WindowConfig::validate` stays where it is; a child module may call it.
- The settings are flat keys of `QuotaConfig`, so `idlemax config` lists each one (`config/settings.rs:44-63`) and `idlemax config set` changes it (`config/edit.rs:45-64`), which is how D18 item 1 wants hard limits changed.
  `QuotaConfig` gets a hand-written `Default`, because its derived one would set every percent to 0.

  ```rust
  pub owner_reserve_pct: f64,        // 50.0, D10.2
  pub safety_margin_pct: f64,        // 10.0, D10.2, A25
  pub five_hour_stop_pct: f64,       // 60.0, D10.4, A18
  pub last_day_floor_pct: f64,       // 15.0, D10.5
  pub seed_small_pct: f64,           // 3.0, percent of the weekly window one unmeasured small Run is charged
  pub seed_standard_pct: f64,        // 8.0, the same for a standard Run
  pub small_task_types: Vec<String>, // ["broken_check"], D10.6, A24
  pub claude_small_model: Option<String>,    // "haiku", A21
  pub claude_standard_model: Option<String>, // "sonnet", A21
  pub codex_small_model: Option<String>,     pub codex_standard_model: Option<String>,    // unset, A21
  pub agy_small_model: Option<String>,       pub agy_standard_model: Option<String>,      // unset
  pub opencode_small_model: Option<String>,  pub opencode_standard_model: Option<String>, // unset
  ```

- The two seeds are the present estimates for `TokenClass::Small` and `Medium` (`crates/idlemax-core/src/scheduler/blockers.rs:81-87`), accepted as starting values (A25).
  They are settings so the owner can replace them with the points per run of Task 1 without a build.
- A provider whose key for a tier is unset cannot be used for that tier, and the reason names the key.
- The existing test builds `QuotaConfig { accounts: .. }` with no rest (`crates/idlemax-core/src/quota/rules/tests.rs:68-73`), which stops compiling once the struct has more fields; that literal gains `..QuotaConfig::default()` and nothing else in the file changes.
- `Provider` (`quota.rs:69-77`) gains `Opencode` (wire value `opencode`): `cli()` returns `"opencode"`, the default probe is `Manual`, and it has no default window, because nothing reads one (D10.8).
  agy keeps its default windows (`quota.rs:100-111`), which old AI jobs use.
  The three `match` blocks on `Provider` all live in `quota.rs` (`:81-110`), so no other file needs an arm.
- `AccountConfig` (`quota.rs:114-126`) gains `daily_cap_runs: Option<u32>`, valid only on `agy` and `opencode` accounts and never 0 (D10.8).
- `QuotaConfig::validate` gains five typed errors on `QuotaConfigError`: a percent setting outside 0 to 100 (names the key), a cap on a `claude`, `codex` or `api` account, a cap of 0, an entry of `small_task_types` that does not parse as Spec 1's `TaskSource` (`broken_check`, `roadmap`, `github_issue`, `session`), and a `claude_small_model` or `claude_standard_model` whose value contains `opus` in any case, because Opus orchestrates only (D10.6, the alias `PASS_MODEL` of Spec 1).
- `tier.rs` holds the tier rule of D10.6 as pure functions:

  ```rust
  #[serde(rename_all = "snake_case")]
  pub enum Tier { Small, Standard }
  pub enum TierPick { Run(Tier), NeedsYou }
  /// 0 failed checks: Small when the task type is in `small_task_types`, else Standard.
  /// 1 failed check: Standard. 2 or more: NeedsYou.
  pub fn tier_for(task_type: &str, failed_checks: u32, cfg: &QuotaConfig) -> TierPick;
  pub fn model_for(provider: Provider, tier: Tier, cfg: &QuotaConfig) -> Option<&str>;
  /// The setting that names this model, such as `quota.codex_small_model`; None for `api`.
  pub fn model_key(provider: Provider, tier: Tier) -> Option<&'static str>;
  /// Every (provider, model) a tier key sets, small before standard.
  pub fn tier_models(cfg: &QuotaConfig) -> Vec<(Provider, String)>;
  /// The tier whose key holds `model` for this provider; Standard when both hold it, None for another name.
  pub fn tier_of(provider: Provider, model: &str, cfg: &QuotaConfig) -> Option<Tier>;
  ```

- `plan_reload` (`config/reload.rs:61-67`) reports every changed `quota.` key other than `quota.accounts` under `applied`, as Spec 1 does for `dispatcher.*` and Spec 2 for `workers.*`, because the coordinator reads them through Spec 1's `LiveConfig` each time it computes a Budget (A2, Task 6).
  `quota.accounts` stays under `restart_agent`, as today: the accounts themselves are still read at start.
- `KNOWN_TOOLS` (`crates/idlemax/src/agent/metrics/caps.rs:7-12`) gains `"opencode"` after `"agy"` so a device reports the CLI.
- Docs in this task: the provider table and the settings table in `README.md` (rows at `README.md:768-773` and `README.md:1005`), the sentence there that lists what applies at once, the `Provider` and `Tier` values under "Encodings" in `docs/API.md`, and the new keys as commented lines in `examples/config.toml`.

**Definition of Done:**

- [x] `Config::parse` accepts `provider = "opencode"` with `daily_cap_runs = 5`, and refuses `daily_cap_runs` on a claude account with a message that names the account and says claude and codex accounts are paced by their readings.
- [x] `Config::parse` refuses `five_hour_stop_pct = 140` with a message that names `quota.five_hour_stop_pct`, `small_task_types = ["broken-check"]` with a message that lists the four sources, and `claude_standard_model = "claude-opus-4"` with a message that says Opus orchestrates only.
- [x] `list_settings("", Os::Macos)` lists `quota.five_hour_stop_pct` with value 60, `quota.claude_small_model` with value `haiku`, `quota.claude_standard_model` with value `sonnet` and `quota.codex_small_model` with value null.
- [x] `tier_for` returns `Run(Small)` for (`broken_check`, 0), `Run(Standard)` for (`roadmap`, 0), `Run(Standard)` for (`broken_check`, 1) and for (`roadmap`, 1), and `NeedsYou` for any type with 2.
- [x] `tier_models` of the default config is `[(Claude, "haiku"), (Claude, "sonnet")]`, and `tier_of(Claude, "sonnet", ..)` is `Standard`.
- [x] A change of `quota.safety_margin_pct` shows under `applied` in the reload report, and a change of `quota.accounts` under `restart_agent`.
- [x] Verify: `cargo test -p idlemax-core quota`, `cargo test -p idlemax-core config`, `cargo test -p idlemax --test examples`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 3: Build the cost table from Run records

**Objective:** Turn the Run records of Spec 2 into a table that says, per provider, account, model and task type, what percent of each window one run costs.
The table gives workers a measured cost and says plainly what it uses before it has data.

**Files:**

- Create: `crates/idlemax-core/src/quota/cost.rs`
- Create: `crates/idlemax-core/src/quota/cost/tests.rs`
- Create: `crates/idlemax/src/coord/store/budget.rs`
- Create: `crates/idlemax/src/coord/store/budget/tests.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax-core/src/report/quota.rs`
- Modify: `crates/idlemax-core/src/scheduler/blockers.rs`
- Modify: `crates/idlemax/src/coord/store.rs`

**Key Decisions / Notes:**

- The pure input is its own small type, so this module does not depend on Spec 2's `Run` type (D10.1):

  ```rust
  pub struct RunSample {
      pub provider: Provider, pub account: String, pub model: String, pub task_type: String,
      pub started_ms: UnixMs, pub ended_ms: Option<UnixMs>,   // None while the Run is active
      pub launches: u32,                                       // Spec 2's count of jobs started for the Run
      pub before: Vec<QuotaSnapshot>, pub after: Vec<QuotaSnapshot>,  // at most one per window
  }
  pub struct RunCost { pub sample: usize, pub window: QuotaWindowKind, pub pct: Option<f64> }
  pub struct CostKey { pub provider: Provider, pub account: String, pub model: String, pub task_type: String }
  #[serde(rename_all = "snake_case")]
  pub enum CostBasis { Measured, SameModel, Seed }
  pub struct CostEstimate { pub pct: f64, pub basis: CostBasis, pub samples: u32 }
  /// One tier of one account as the owner sees it: the estimate over every task type of the model.
  pub struct TierCost { pub tier: Tier, pub model: String, pub est_pct: f64, pub basis: CostBasis,
                        pub samples: u32, pub five_hour_pct: Option<f64> }
  /// A map from `CostKey` and window to the newest 20 measured costs.
  pub struct CostTable { /* private */ }
  pub fn run_costs(samples: &[RunSample]) -> Vec<RunCost>;
  pub fn cost_table(samples: &[RunSample], costs: &[RunCost]) -> CostTable;
  /// What one Run of this key is expected to cost, in percent of the weekly window.
  pub fn expected(table: &CostTable, key: &CostKey, tier: Tier, cfg: &QuotaConfig) -> CostEstimate;
  /// One line per tier whose model key is set.
  pub fn tier_costs(provider: Provider, account: &str, table: &CostTable, cfg: &QuotaConfig) -> Vec<TierCost>;
  ```

- A cost is measured on a window only when the Run has ended, was launched once, the before reading was taken at most 6 hours before the start (`SNAPSHOT_MAX_AGE_MS`, `quota/rules.rs:12`), the after reading was taken at or after the end, and both name the same reset time within 5 minutes.
  Otherwise `pct` is `None`, and the Run is charged at its estimate wherever a cost is needed, never at zero.
- A Run launched more than once sat `shed` in between, and a rise of the reading in that gap is not its own; such a Run is never a sample.
- Where the two readings come from: the before reading is the Run's stored `quota_before`.
  The after reading is not the Run's stored `quota_after`: Spec 2 stores the newest snapshot it holds when the launch ends, and a snapshot is dated by the capture file's modified time (`crates/idlemax/src/agent/quota_probe.rs:189`) and never after the coordinator's clock (`crates/idlemax-core/src/quota.rs:265-270`), so that reading is always older than the Run's end.
  `run_samples_since` therefore takes, for each window, the oldest row of `quota_snapshots` for the account that was taken at or after the Run's end.
  A Run with no such row yet stays unmeasured until one arrives.
- Overlapping Runs on one account share the reading: time is cut at every start and end, and the rise of the reading across a segment is split equally among the Runs active in it.
  The reading at a cut is the before or after reading of the Run that starts or ends there, and a fall counts as 0.
- A table row is the mean of the newest 20 measured costs of one key and window; the mean is used because a reading may move in whole points while a run costs less than one.
- `expected` looks up the weekly window in this order: the exact key with at least 5 samples (`Measured`), every task type of the same provider, account and model with at least 5 samples (`SameModel`), then the tier's seed setting, `quota.seed_small_pct` or `quota.seed_standard_pct` (`Seed`).
- Only the weekly estimate gates anything, because the 5-hour line of Task 5 is a reading and not an estimate; so the 5-hour window has no seed, and `TierCost.five_hour_pct` is the 5-hour mean of the model once it has 5 samples and empty before.
- Samples come from the Runs of the last `coordinator.metrics_retention_days` (7 by default, `crates/idlemax-core/src/config.rs:74`), because older snapshots are pruned (`crates/idlemax/src/coord/store.rs:218-223`) and with them the after readings.
- The cycle rule moves to one place: `SAME_RESET_MS` (private in `crates/idlemax-core/src/report/quota.rs:16`) becomes `pub const SAME_RESET_MS` in `quota.rs`, and `report/quota.rs` uses it from there.
- `token_class_pct` (`scheduler/blockers.rs:79-87`) keeps its three figures, because old AI jobs keep their fixed cost figures (A8).
  Only its `SHORTCUT` comment is reworded to `// SHORTCUT: fixed estimates for old AI jobs only (workers use quota::cost), measure them once an old AI job runs as a Run.`
- `run_samples_since(since_ms)` in `coord/store/budget.rs` selects the rows of Spec 2's `runs` table whose start is at or after `since_ms` and maps each to a `RunSample`: provider, account, model, the task source as the task type (A24), start, end, `launches` and `quota_before`, with `after` filled from `quota_snapshots` as said above.
  It reads only.

**Definition of Done:**

- [x] A Run alone on its account costs its after reading minus its before reading on each window.
- [x] Run A from minute 0 to 10 and Run B from minute 5 to 15 on one account, with weekly readings 40.0, 41.0, 43.0 and 44.0 at those four moments, each cost 2.0.
- [x] A Run whose after reading is older than its end has `pct` `None`, a Run with `launches` of 2 has `pct` `None`, and `expected` for either key still returns the seed.
- [x] With 5 measured samples of 1.0, 2.0, 1.0, 2.0 and 1.5 for a key, `expected` returns 1.5 with basis `Measured`; with 4 it returns 8.0 for the standard tier with basis `Seed`; with 5 samples spread over two task types of one model it returns their mean with basis `SameModel`.
- [x] `tier_costs` for a claude account with the default config has a `small` line with `haiku` and a `standard` line with `sonnet`, each with its estimate, basis and sample count.
- [x] `grep -rnE '(#|//) ?SHORTCUT:' crates/idlemax-core/src/scheduler/blockers.rs` shows one line that names old AI jobs, and `token_class_pct` still returns 3.0, 8.0 and 20.0.
- [x] Store test: for a Run stored through Spec 2's store methods whose `quota_after` is older than its end, and one weekly snapshot inserted after its end, `run_samples_since` returns the sample with that later snapshot as its after reading; with no later snapshot its `after` is empty.
- [x] Verify: `cargo test -p idlemax-core quota::cost`, `cargo test -p idlemax-core scheduler`, `cargo test -p idlemax-core report`, `cargo test -p idlemax --bin idlemax store::budget`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 4: Learn the owner's own use per account day

**Objective:** Work out, for each account and each finished account day, how much of the weekly window was used in all, how much idlemax's Runs took, and how much is left as the owner's own use.
Keep these days, and later the budget each day had, in the store for 30 days, because the raw snapshots are pruned after one week.

**Files:**

- Create: `crates/idlemax-core/src/quota/owner_use.rs`
- Create: `crates/idlemax-core/src/quota/owner_use/tests.rs`
- Create: `crates/idlemax/src/coord/api/budget.rs`
- Create: `crates/idlemax/src/coord/api/budget/tests.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax/src/coord/store/schema.rs`
- Modify: `crates/idlemax/src/coord/store/budget.rs`
- Modify: `crates/idlemax/src/coord/store/budget/tests.rs`
- Modify: `crates/idlemax/src/coord/store.rs`
- Modify: `crates/idlemax/src/coord/api.rs`
- Modify: `crates/idlemax/src/coord/sweep.rs`

**Key Decisions / Notes:**

- The pure part (D10.2, first sentence):

  ```rust
  pub const DAY_MS: i64 = 86_400_000;
  pub struct DaySlot { pub start_ms: UnixMs, pub end_ms: UnixMs, pub days_left: u32 }
  /// days_left = ceil((resets_at_ms - now_ms) / DAY_MS), at least 1; the slot ends
  /// (days_left - 1) days before the reset.
  pub fn day_slot(resets_at_ms: UnixMs, now_ms: UnixMs) -> DaySlot;
  /// The UTC day of `now_ms`, for an account with no reset time; days_left is 1.
  pub fn utc_day_slot(now_ms: UnixMs) -> DaySlot;
  pub fn closed_slots(weekly: &[QuotaSnapshot], now_ms: UnixMs) -> Vec<DaySlot>;
  /// One row of `quota_days`. The three budget fields are what the day's Budget last said
  /// (Task 6 writes them); `basis` is the wire text of the Budget's basis.
  pub struct AccountDay { pub account: String, pub day_start_ms: UnixMs,
                          pub spendable_pct: Option<f64>, pub basis: Option<String>, pub why: Option<String>,
                          pub total_pct: Option<f64>, pub idlemax_pct: Option<f64>,
                          pub owner_pct: Option<f64>, pub runs: Option<u32>, pub closed: bool }
  pub fn account_day(account: &str, slot: &DaySlot, weekly: &[QuotaSnapshot],
                     charges: &[(UnixMs, f64)]) -> AccountDay;
  pub struct OwnerUse { pub days_known: u32, pub usual_daily_pct: Option<f64> }
  pub fn owner_use(days: &[AccountDay]) -> OwnerUse;
  ```

- `account_day` returns a closed day with its three budget fields empty.
  `total_pct` is the weekly reading at the slot's end minus the reading at its start, both from snapshots of one cycle (same reset within `SAME_RESET_MS`, as `report/quota.rs:83-111` groups them).
  The start reading is the newest snapshot at or before the slot's start, or 0 for the first slot of a cycle; the end reading is the newest snapshot inside the slot; a slot with no snapshot inside has `total_pct` `None`.
- `charges` are (end time, percent of the weekly window) of the Runs that ended in the slot, measured where Task 3 measured them and at their estimate otherwise.
  `idlemax_pct` is their sum, `runs` their number, and `owner_pct` is `total_pct` minus `idlemax_pct`, never below 0.
- `owner_use` is the mean `owner_pct` of the newest 7 closed days that have one, and `days_known` is how many of those exist, at most 7.
- Migration: the next `Vn` in `crates/idlemax/src/coord/store/schema.rs`.
  `MIGRATIONS` ends at `V7` today (`schema.rs:4`); Specs 1 and 2 each add one, so `V10` is the number to expect, and the implementer reads `MIGRATIONS` on `main` first and takes the next free one.

  ```sql
  CREATE TABLE quota_days (
    account TEXT NOT NULL,
    day_start_ms INTEGER NOT NULL,
    spendable_pct REAL,
    basis TEXT,
    why TEXT,
    total_pct REAL,
    idlemax_pct REAL,
    owner_pct REAL,
    runs INTEGER,
    closed INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account, day_start_ms)
  ) STRICT, WITHOUT ROWID;
  ```

- The table exists because snapshots are pruned after `coordinator.metrics_retention_days`, 7 by default (`crates/idlemax-core/src/config.rs:74`, `crates/idlemax/src/coord/store.rs:218-223`), which is exactly the week D10.2 needs, and because the shared Budget is "per account and day": Spec 4 and Task 12 read what a past day's budget was.
- Store methods in `coord/store/budget.rs`: `weekly_snapshots(account, since_ms)`; `close_day(&AccountDay)`, an upsert that sets the four totals and `closed = 1` and keeps the three budget columns; `record_open_day(account, day_start_ms, spendable_pct, basis, why)`, an upsert that sets the three budget columns only while `closed = 0`; `budget_days(account, since_ms)`, newest first; and `runs_started_in(account, from_ms, to_ms)`, the number of the account's Runs with a session id that started in that span.
- `close_quota_days(store, cfg, now)` in `coord/api/budget.rs` closes every ended slot that is not closed yet.
  For an account read through its weekly window the slots come from `closed_slots` and the row from `account_day`.
  For an account on a daily cap it closes each open row whose UTC day has ended: `runs` is `runs_started_in` for that day, `idlemax_pct` is 100 times `runs` divided by the cap, and `total_pct` and `owner_pct` stay empty.
  `sweep_once` (`coord/sweep.rs:71-84`) calls it in its hourly branch before `store.prune`, and it is exported from `coord/api.rs` the way `run_refresh` is (`api.rs:161`).
  A second call changes nothing.
- `Store::prune` (`store.rs:208-262`) drops `quota_days` rows older than `HISTORY_RETENTION_MS` (`store.rs:34`), and `PruneReport` (`store.rs:120-132`) counts them.

**Definition of Done:**

- [x] With the reset 49 hours away, `day_slot` returns `days_left` 3 and a slot that ends in 1 hour.
- [x] Readings of 20.0 at the slot's start and 31.0 inside it, with charges of 4.0, give `total_pct` 11.0, `idlemax_pct` 4.0 and `owner_pct` 7.0.
- [x] A slot with no snapshot inside has `owner_pct` `None` and does not count in `days_known`.
- [x] Seven days with owner use of 7, 5, 0, 9, 6, 8 and 7 give `usual_daily_pct` 6.0 and `days_known` 7.
- [x] A fresh store has the `quota_days` table, two calls of `close_quota_days` leave one closed row per account day, and `prune` removes a row older than 30 days.
- [x] A day recorded with `record_open_day` and then closed keeps its `spendable_pct` and `why`, a later `record_open_day` for that day changes nothing, and `budget_days` returns it first among older days.
- [x] `sweep_once` with the hourly flag set closes an ended account day.
- [x] Verify: `cargo test -p idlemax-core quota::owner_use`, `cargo test -p idlemax --bin idlemax store::budget`, `cargo test -p idlemax --bin idlemax coord::api::budget`, `cargo test -p idlemax --bin idlemax sweep`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 5: Compute the surplus, the daily Budget and the worker rules

**Objective:** Compute, as pure functions with `now_ms`, how much of an account's weekly quota would expire unused, and from it the Budget for today with the reason for its number.
This builds the reserve, the daily pacing, the 5-hour stop, the last-day sweep, the fixed daily cap and the rest after a limit, and the one module that says which rules apply to a worker.

**Files:**

- Create: `crates/idlemax-core/src/quota/surplus.rs`
- Create: `crates/idlemax-core/src/quota/budget.rs`
- Create: `crates/idlemax-core/src/quota/budget/tests.rs`
- Create: `crates/idlemax-core/src/quota/spend.rs`
- Create: `crates/idlemax-core/src/quota/spend/tests.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax-core/src/quota/rules.rs`

**Key Decisions / Notes:**

- The shared name Budget, with the fields other specs rely on (`spendable_pct`, `spent_pct`, `why`) and what this plan adds:

  ```rust
  #[serde(rename_all = "snake_case")]
  pub enum BudgetUnit { WeeklyPct, DailyCapRuns }
  #[serde(rename_all = "snake_case")]
  pub enum BudgetBasis { Learned, Reserve, LastDay, Measuring, DailyCap,           // usable
                         Paused, Resting, NoData, NoCap, NotForWorkers, FiveHourStop, Spent }
  pub struct Budget {
      pub account: String, pub provider: Provider, pub unit: BudgetUnit,
      pub day_start_ms: UnixMs, pub day_end_ms: UnixMs,
      pub spendable_pct: f64, pub spent_pct: f64, pub left_pct: f64,
      pub usable: bool, pub rests_until_ms: Option<UnixMs>, pub owner_active: bool,
      pub cap_runs: Option<u32>, pub runs_today: u32,
      pub costs: Vec<TierCost>,
      pub basis: BudgetBasis, pub why: String,
  }
  pub struct BudgetInput<'a> {
      pub now_ms: UnixMs, pub account: &'a AccountConfig, pub cfg: &'a QuotaConfig,
      pub windows: &'a [QuotaView],                 // this account's rows from quota::views
      pub owner: OwnerUse, pub idlemax_week_pct: f64,
      pub spent_today_pct: f64, pub committed_pct: f64, pub runs_today: u32,
      pub measuring_left_pct: Option<f64>,          // Spec 2's measuring week (A7); None once it is over
      pub owner_reserve_override_pct: Option<f64>,  // Spec 5: a server-only account
      pub limit_hit_ms: Option<UnixMs>,             // the newest stored limit hit of this account
      pub paused: bool,
      pub last_owner_use_ms: Option<UnixMs>,        // the owner's newest use of this provider's CLI
      pub costs: &'a [TierCost],                    // from tier_costs (Task 3), copied into the Budget
  }
  pub fn budget(input: &BudgetInput<'_>) -> Budget;
  ```

- The owner's hold, in `surplus.rs` (D10.2): the owner's use this week is the weekly reading minus `idlemax_week_pct`, never below 0.
  The learned part is `usual_daily_pct` times the days until the reset, as a fraction.
  The floor is `owner_reserve_pct` minus the owner's use this week, never below 0, and it applies while `days_known` is under 7.
  The hold is the larger of the two; with `owner_reserve_override_pct` set, the hold is that figure minus the owner's use this week and nothing is learned.
- The surplus is 100 minus the weekly reading, minus the hold, minus `safety_margin_pct`, never below 0 (D10.2).
- Paced by day (D10.3): `spendable_pct` is the surplus plus `spent_today_pct`, divided by `days_left` of `day_slot` (Task 4); adding back today's spend keeps the day's figure steady while it is being spent.
- Last day (D10.5): when `days_left` is 1, `spendable_pct` is 100 minus the weekly reading minus `last_day_floor_pct`, never below 0, plus `spent_today_pct`.
- The measuring week (A7): `spendable_pct` is never more than `spent_today_pct` plus `measuring_left_pct`, so the Budget takes the smaller of the two figures.
- `left_pct` is `spendable_pct` minus `spent_today_pct` minus `committed_pct`, never below 0; `committed_pct` is the estimated cost of the Runs still active on the account, so a running Run holds its whole estimate until it ends.
- The fixed daily cap (D10.8): an account with `daily_cap_runs` gets `unit` `daily_cap_runs`, `cap_runs` set, `spendable_pct` 100, `spent_pct` equal to 100 times `runs_today` divided by the cap, and `left_pct` equal to the rest.
  The unit is Runs started per day, because a start is the only thing idlemax can count by itself for these CLIs: no reader gives a percent or tokens for them, and one Run is one task in one fresh session (D10.9), so a Run is a bounded amount.
  No reset time is known for these accounts, so their account day is `utc_day_slot`.
  The surplus, the 5-hour stop and the last-day rule do not apply to them, because there is nothing to read; a pause and a rest still close the account.
- `rests_until_ms` is `limit_rest_until` (below) for `limit_hit_ms`, and `owner_active` is true when `last_owner_use_ms` is within `INTERACTIVE_HOLD_MS` (30 minutes, `quota/rules.rs:10`) of now.
- The first rule that closes the account sets `usable` to false, the `basis` and the `why`, in this order: `not_for_workers` (the provider is `api`), `no_cap` (an `agy` or `opencode` account without `daily_cap_runs`, with the `why` `set daily_cap_runs for this account in [[quota.accounts]]`), `paused`, `resting`, `no_data` (the weekly row is not `Active` with a reset time, or a 5-hour row is `Stale`), `five_hour_stop` (the 5-hour reading is at or above `five_hour_stop_pct`, D10.4), `spent` (`left_pct` is 0).
  The numbers are still filled in when the account is closed, so the owner sees what the day's budget is; with no reset time known the day shown is `utc_day_slot`.
- `why` is one sentence that carries the numbers the result came from; the private `duration` helper of `quota/rules.rs:313-322` becomes `pub(super)` and words the times.
- The worker rules live in one place, `spend.rs`:

  ```rust
  pub struct Gate { pub allowed: bool, pub reason: String }
  /// May one worker Run with this estimate start on the account now.
  pub fn gate(budget: Option<&Budget>, est_pct: f64) -> Gate;
  /// The bases at which the account's running workers stop with their state saved.
  pub fn stops_workers(basis: BudgetBasis) -> bool;
  /// The usable Budgets in the order given, the ones the owner is not using first.
  pub fn offers(budgets: &[Budget]) -> Vec<&Budget>;
  pub const LIMIT_USED_UP_PCT: f64 = 90.0;
  /// Until when a limit hit at `hit_ms` rests the account; None when it rests no longer.
  pub fn limit_rest_until(account: &AccountConfig, windows: &[QuotaView],
                          hit_ms: UnixMs, now_ms: UnixMs) -> Option<UnixMs>;
  /// `claude-max: usage limit hit, workers rest for 3h 10m`.
  pub fn rest_event(account: &str, until_ms: UnixMs, now_ms: UnixMs) -> String;
  ```

- `gate` refuses a missing Budget, refuses a Budget that is not `usable` with its `why`, and refuses an estimate above `left_pct` with both numbers (`needs 8.0%, 5.3% left today`); so Truth 1 holds at the start itself and not only when the pass picks.
- `stops_workers` is true for `resting` (D10.7) and `five_hour_stop` (A18) and false for every other basis.
  It is false for `spent`, because a running Run holds its own estimate in `committed_pct` and would stop itself, and for `no_data`, because the running workers' own sessions are what refreshes the reading (Open Question 1).
- `offers` only orders: it never drops a usable Budget, and it compares no `left_pct` across accounts, since the two units differ (D10.4 says "prefers").
- `limit_rest_until` (D10.7) looks at the windows in whose running cycle the hit fell.
  Among them it takes the ones with trusted data at or above `LIMIT_USED_UP_PCT`, else the shortest; the rest lasts until the latest reset among those.
  With no reset known it lasts one length of the account's shortest window after the hit, and 300 minutes when the account has no window.
  For an account on a daily cap it lasts until the end of the UTC day of the hit.
  This function is for workers only: `limit_block` (`quota/rules.rs:277-303`) keeps its own rule for old AI jobs and is not edited (A8).
- The module doc of `spend.rs` is the table of which rule applies to an old AI job and which to a worker; Task 6 puts the same table into `README.md`.

**Definition of Done:**

- [x] Learned: weekly reading 40, usual use 6 a day with 7 days known, reset in 72 hours, margin 10, nothing spent today: `spendable_pct` is 32 divided by 3, and the basis is `learned`.
- [x] Reserve: 3 days known with usual use 4, weekly reading 20 of which idlemax spent 8 on earlier days (so the owner's use this week is 12), reset in 96 hours, margin 10, reserve 50: the hold is 38, `spendable_pct` is 8.0, and the basis is `reserve`.
- [x] Last day: weekly reading 70, reset in 20 hours, floor 15, 5 spent today: `spendable_pct` is 20.0, `left_pct` is 15.0, and the basis is `last_day`.
- [x] Measuring: the learned case with `measuring_left_pct` 4 gives `spendable_pct` 4.0 and the basis `measuring`.
- [x] A 5-hour reading of 62 gives `usable` false, basis `five_hour_stop`, and the `why` `5h window 62% used, stop line 60%: no worker runs until it resets`.
- [x] A weekly row in state `no_data` gives `usable` false with basis `no_data`, and `spent_today_pct` equal to `spendable_pct` gives basis `spent` with a `why` that names the start of the next account day.
- [x] `owner_reserve_override_pct` of 5 with the owner's use this week at 0 gives a hold of 5.
- [x] A cap of 5 with 3 Runs today gives `spendable_pct` 100, `spent_pct` 60, `left_pct` 40, `usable` true, basis `daily_cap` and the `why` `daily cap: 3 of 5 runs used today`; 5 of 5 gives basis `spent`; an opencode account without a cap gives basis `no_cap`.
- [x] `owner_active` is true for a last use 10 minutes ago and false for 40 minutes ago.
- [x] `gate` refuses a missing Budget, refuses an estimate of 8.0 against `left_pct` 5.3 with both numbers, allows 3.0, and refuses a closed Budget with its `why`.
- [x] `stops_workers` is true for `resting` and `five_hour_stop` and false for the other ten bases.
- [x] `offers` of a usable account the owner is using and a usable one the owner is not using returns both, the second first; an unusable Budget is never returned.
- [x] With the weekly window at 97 percent resetting in 30 hours and the 5-hour window at 40 percent, a hit now rests the account until the weekly reset; with the weekly at 50 and the 5-hour at 100 it rests until the 5-hour reset; with no reset known it rests 300 minutes; a hit from before the running cycle rests nothing; a Budget with such a hit has basis `resting` and `rests_until_ms` set.
- [x] Verify: `cargo test -p idlemax-core quota::budget`, `cargo test -p idlemax-core quota::spend`, `cargo test -p idlemax-core quota::rules`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 6: Gather the Budget on the coordinator and gate every worker start

**Objective:** Add the coordinator use case that gathers the inputs and returns the Budget of every account, and make every worker start of Spec 2 ask it.
Today's rules for AI jobs would almost never let a worker start, so this task also writes down which rule applies to which kind of work.

**Files:**

- Modify: `crates/idlemax/src/coord/api/budget.rs`
- Modify: `crates/idlemax/src/coord/api/budget/tests.rs`
- Modify: `crates/idlemax/src/coord/sweep.rs`
- Modify: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `README.md`

**Key Decisions / Notes:**

- `budgets(store, cfg, live, views, now)` in `coord/api/budget.rs` returns one Budget per entry of `[[quota.accounts]]`, in that order.
  It follows `gather_plan(store, cfg, live, now)` of Spec 1: `cfg` is the coordinator's start config and gives the accounts, and `live` is `LiveConfig::current()` and gives every other `[quota]` key, so a changed setting applies to the next call with no restart (A2) and a new account needs a coordinator restart, as today.
  It takes the views as an argument, so a caller that already ran `quota_views` (`coord/api/jobs.rs:257-277`) does not run it twice under the store lock.
- It reads the account days with `budget_days` (Task 4), the Run samples of the snapshot retention with `run_samples_since` (Task 3), the paused accounts with `Store::paused_scopes` (`coord/store/controls.rs:52`) and `paused_accounts` (`crates/idlemax-core/src/control/mod.rs:74-82`), the limit hits of the last week with `Store::limit_hits` (`coord/store/quota.rs:24`), the owner's CLI use with `Store::interactive_use` (`crates/idlemax/src/coord/store/devices.rs:170-183`), and `runs_started_in` for today.
- The measuring week comes from Spec 2 (A7): while `now` is before `pace::measuring_week_end` of the account's first Run (the oldest row of `runs_of_account`), `measuring_left_pct` is `MEASURING_WEEK_CAP_PCT` minus `pace::week_spend`, never below 0; after that it is `None`, and so it is for an account with no Run yet.
- From the samples it builds the cost table once per call: `spent_today_pct` is the charges of the Runs that ended in today's slot, `idlemax_week_pct` the charges since the weekly cycle began, and `committed_pct` the estimates of the Runs with no end time, each with `expected` and the tier `tier_of` gives its model, or the standard tier for a model no key holds.
- `budgets` is not called from `quota_views`, because every heartbeat calls that under the store lock (`coord/api/agents.rs:156`); only the plan, the worker start, the hourly sweep and the quota route call it.
- `worker_gate(store, cfg, live, views, account, model, task_type, now)` is the one call a start makes: it takes the account's Budget from `budgets`, the Run's estimate from `expected` (100 divided by the cap for an account on a daily cap) and returns `gate` of the two (Task 5).
- The start side of Spec 2 in `crates/idlemax/src/coord/dispatch.rs`, where the account rule is `account_uses` today:
  1. In `launch_due`, the condition "the account is usable by `account_uses`" of `pace::may_launch` is answered by `worker_gate`, for a pick and for the relaunch of a `shed` run.
     Spec 2 already skips a run or a pick that may not launch and tries the next one, so a run whose account is closed never holds up the device, and the refusal's text is the reason it records.
     A worker's next task is a new Run through `launch_due`, so the same gate covers it.
  2. The reviewer, "the first account of another provider that `account_uses` calls usable and whose CLI is in `workers.reviewers`", is the first such account whose `worker_gate` allows a Run of the reviewer's model.
  The site that stops live workers keeps `account_uses` until Task 9.
- `record_open_days(store, cfg, live, now)` runs `quota_views` and `budgets` and writes each Budget's `spendable_pct`, `basis` and `why` with `record_open_day` (Task 4); `sweep_once` calls it in its hourly branch after `close_quota_days`, with the live config it hands `launch_due`.
  So a closed day keeps the last budget recorded within its final hour.
- The rules, as a table in `README.md` in place of the list at `README.md:776-784`, the same ten rows as the module doc of `spend.rs`:

  | Rule (where it lives) | Old AI job | Worker |
  |---|---|---|
  | The device lists the account (`scheduler/blockers.rs:211-215`) | applies | applies: Spec 1 picks only an account in the device's `ai_accounts` |
  | `idlemax pause --account` (`scheduler/blockers.rs:173-175`) | applies | applies, basis `paused`; a running worker is frozen in place (A24) |
  | Usage data is trusted and under 6 hours old (`quota/rules.rs:143-164`) | applies | applies to a start, basis `no_data` |
  | A usage limit or HTTP 429 was seen (`quota/rules.rs:277-303`) | applies, unchanged | the rest of D10.7, basis `resting`; running workers stop |
  | Reserve of 40 percent per 5-hour window, 30 weekly, 15 in the final day (`quota.rs:15-18`) | applies | replaced by the surplus, the 5-hour stop at 60, where running workers stop too (A18), and the last-day floor of 15 |
  | Only in the final third of the shortest window or within 24 hours of the weekly reset (`quota/rules.rs:240-263`) | applies | does not apply: the Budget paces by account day (D10.3) |
  | Hold for 30 minutes after interactive use (`quota/rules.rs:265-275`) | applies | does not apply (D4); it becomes the preference of Task 8 (D10.4) |
  | One AI lease per account (`quota/rules.rs:208-210`, `coord/store/schema.rs:61-62`) | applies among old AI jobs | does not apply, in both directions (A8, built by Spec 2) |
  | One AI job per account per 5 hours (`quota/rules.rs:305-311`) | applies among old AI jobs | does not apply, and a worker start does not begin the span (A8, built by Spec 2) |
  | Headroom for the job's token class (`scheduler/blockers.rs:176-184`) | applies, with the fixed figures (A8) | replaced by: the Run's estimate fits in `left_pct` |

  Under the table one sentence says that the orchestrator pass of Spec 1 is an old AI job that skips the headroom row and, from Task 10 on, is queued only while its account's Budget is usable.
- `README.md` also gains the daily cap and its unit in the "AI quota maximizer" section.

**Definition of Done:**

- [x] `budgets` on an in-memory store with one claude and one api account returns two Budgets in config order, the second with basis `not_for_workers`.
- [x] `budgets` takes `safety_margin_pct` from `live` and the accounts from `cfg`: a live config with another margin changes `spendable_pct`, and an account that only the live config lists gets no Budget.
- [x] With a limit hit stored for an account whose weekly window is at 97 percent, its Budget has basis `resting` and `rests_until_ms` at the weekly reset.
- [x] An account whose first Run started 3 days ago and whose `week_spend` is 6 has basis `measuring` and `spendable_pct` of at most `spent_pct` plus 4; with the first Run 8 days ago the cap is gone.
- [x] For an account on a daily cap, `runs_today` counts a Run with a session id that started today, and counts neither a Run that started before the UTC day began nor a Run with no session id.
- [x] `worker_gate` refuses a start whose estimate is above `left_pct` with both numbers and allows one that fits.
- [x] `launch_due` starts no worker on an account whose gate refuses and still launches the next pick on another account; a `shed` run on a refused account is passed over and a later pick launches.
- [x] A reviewer account whose Budget has basis `five_hour_stop` is not chosen.
- [x] After `record_open_days`, `budget_days` shows today's row with `spendable_pct`, `basis` and `why`.
- [x] The rule table in `README.md` has the same ten rows as the module doc of `spend.rs`.
- [x] Verify: `cargo test -p idlemax --bin idlemax coord::api::budget`, `cargo test -p idlemax --bin idlemax coord::dispatch`, `cargo test -p idlemax --bin idlemax store::budget`, `cargo test -p idlemax --bin idlemax sweep`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 7: Leave idlemax's own worker sessions out of the owner's use

**Objective:** Make the agent report when the owner last used each CLI without counting idlemax's own worker sessions, so `owner_active` of a Budget means the owner.
This also ends the effect, named in roadmap item 11, that a worker's session lengthens the interactive hold of old AI jobs.

**Files:**

- Create: `crates/idlemax/src/agent/quota_probe/own_sessions.rs`
- Modify: `crates/idlemax/src/agent/quota_probe.rs`
- Modify: `crates/idlemax/src/agent/probes.rs`
- Modify: `docs/API.md`
- Test: `crates/idlemax/src/agent/quota_probe/tests.rs`

**Key Decisions / Notes:**

- `interactive_use` (`crates/idlemax/src/agent/quota_probe.rs:145-171`) takes the newest session file per CLI today, idlemax's own included, as its doc comment says.
  It now skips a session file that belongs to an idlemax worker, so the time it reports is the owner's (D10.4).
- A session file is idlemax's when the last record in its tail that carries a `cwd` (the field `crates/idlemax/src/advisor/sessions.rs:71-79` reads from Claude transcripts) is a path under `Paths::worktrees_dir()` of Spec 2; `ProbeDirs` (`quota_probe.rs:60-72`) gains that folder.
  No marker is added for this (A12): the worker's folder is a fact the session file already holds, while `IDLEMAX_JOB_ID` lives in the pane's environment, which a session file does not record.
- The walk goes through the CLI's session files newest first and stops at the first one that is not a worker's.
  It looks at no more than `OWN_SESSION_WALK` (64) files per CLI per sample; when it reaches that bound it reports the newest file's time, which can only make the owner look active.
  D13 sets no worker count, so a fixed small number of newest files could all be workers' and hide the owner.
- The probe runs on every sample (`crates/idlemax/src/agent/probes.rs:84-89`), so `OwnSessions` remembers for each path whether it is a worker's, because a session's folder does not change.
  A file's tail, the last 64 KiB (`TAIL_START_BYTES`, `quota_probe.rs:18`), is read once; the map is cleared when it passes 1024 entries, and `Probes` holds it.
- `quota_probe.rs` has 391 lines, so the new code lives in `quota_probe/own_sessions.rs`, and `interactive_use` only gains the `OwnSessions` argument.
- For Codex and agy the place of the working folder in a session file is taken from item 9 of `docs/QUOTA-COSTS.md`; until it is proven for a CLI, none of that CLI's sessions is skipped, which can only make the owner look active.
- OpenCode has no known session folder (item 8 of `docs/QUOTA-COSTS.md`), so it reports no interactive use, and its account counts as not in use.
- An old AI job's own `claude -p` session runs outside the worktrees folder and still counts as interactive use, as today; Task 12 records that as the part of roadmap item 11 that is left.
- `docs/API.md` says at `Heartbeat.interactive_use` (`docs/API.md:134`) that the time is the owner's newest session and leaves out idlemax's workers.

**Definition of Done:**

- [x] With an owner's Claude session file and a newer one whose `cwd` is under the worktrees folder, `interactive_use` reports the owner's file time for `claude`.
- [x] With only worker session files, `claude` is absent from the list.
- [x] With six worker session files newer than the owner's, it reports the owner's file time.
- [x] With more worker session files than `OWN_SESSION_WALK`, it reports the newest file's time.
- [x] A file classified once is not read again: after the first call the test empties the worker's file, and the second call still skips it.
- [x] Verify: `cargo test -p idlemax --bin idlemax quota_probe`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 8: Offer accounts and check picks in pure code

**Objective:** Decide in pure code which accounts and models an orchestrator pass may be offered, whether a pass is worth starting, and whether each pick fits the Budget, the tier and the owner's own use.
Nothing calls these functions yet; Task 10 wires them into Spec 1.

**Files:**

- Create: `crates/idlemax-core/src/quota/pass.rs`
- Create: `crates/idlemax-core/src/quota/pass/tests.rs`
- Modify: `crates/idlemax-core/src/quota.rs`

**Key Decisions / Notes:**

- The pure part (D10.4, D10.6, D10.9):

  ```rust
  pub struct TierOffer { pub tier: Tier, pub model: String, pub est_pct: f64, pub basis: CostBasis }
  pub struct AccountOffer { pub account: String, pub provider: Provider, pub unit: BudgetUnit,
                            pub left_pct: f64, pub owner_active: bool, pub tiers: Vec<TierOffer> }
  /// One offer per usable Budget whose CLI may run workers and that has a model for a tier.
  pub fn account_offers(offered: &[&Budget], worker_clis: &[String]) -> Vec<AccountOffer>;
  /// True when an offer has room for one Run of its cheapest tier.
  pub fn any_room(offers: &[AccountOffer]) -> bool;
  /// The quota part of the pass input: at most 8 accounts and a count of the rest.
  pub fn quota_summary(offers: &[AccountOffer]) -> serde_json::Value;
  pub struct PickFacts<'a> { pub account: &'a str, pub model: &'a str,
                             pub task_type: &'a str, pub failed_checks: u32 }
  pub struct Placed { pub model: String, pub tier: Tier, pub est_pct: f64, pub model_changed: bool }
  pub enum PickRefusal { AccountNotOffered, ModelNotSet { key: &'static str }, NeedsYou,      // final
                         OverBudget { est_pct: f64, left_pct: f64 }, OwnerActive { free: String } }  // may pass later
  impl PickRefusal { pub fn is_final(&self) -> bool }   // Display gives the text the owner reads
  pub fn check_picks(picks: &[PickFacts<'_>], offers: &[AccountOffer], table: &CostTable,
                     cfg: &QuotaConfig) -> Vec<Result<Placed, PickRefusal>>;
  ```

- `account_offers` takes the result of `offers` (Task 5) and reads the tiers from `Budget.costs`; for an account on a daily cap one Run costs 100 divided by the cap.
  `worker_clis` is Spec 2's `workers.clis`: a provider whose `Provider::cli()` is not in it gives no offer, so an account on which no worker can start is never offered (A21).
- The tier a task is allowed is `tier_for` (Task 2) with `failed_checks`, the number of that task's Runs whose verdict is `rejected`.
  The model is not the orchestrator's choice once the account is picked: it is `model_for` of the account's provider and that tier.
  `check_picks` therefore sets `Placed.model` and marks `model_changed` when the orchestrator named another model, and never refuses a pick over a model name.
- `check_picks` returns its results in the order of the picks, and checks the picks on accounts the owner is not using first.
  Each placed pick lowers that account's `left_pct` by its estimate, so the picks of one plan cannot overdraw a day.
  The estimate is `expected` (Task 3) for the pick's account, model and task type.
- The preference of D10.4 as a fixed rule: a pick on an account the owner is using is refused with `OwnerActive` only while an offered account the owner is not using still has room for that Run in the same tier; when none has, the pick is placed.
- A pass of Spec 1 may return more picks than there are open slots, and its result is kept until the next pass, so a pick is checked more than once.
  `is_final` tells the two kinds of refusal apart: `AccountNotOffered`, `ModelNotSet` and `NeedsYou` cannot change before the next pass, while `OverBudget` and `OwnerActive` depend on the moment, and a slot that opens later may take such a pick.
- A second attempt is a new Run in a fresh session and never a resume of the failed one (D10.9).
- D10.9 on the quota side: `quota_summary` lists per account its name, provider, unit, `left_pct`, `owner_active` and per tier the model and the estimate, clips each name to 64 characters and stays under 4096 bytes, so the pass input stays short.

**Definition of Done:**

- [x] A `broken_check` task with 0 failed checks is placed on the small model; when the orchestrator named the standard model it is still placed on the small model with `model_changed` true; with 1 failed check it is placed on the standard model; with 2 it is refused with `NeedsYou`.
- [x] Two picks estimated at 4.0 each on an account with `left_pct` 6.0 give one placed pick and one `OverBudget`.
- [x] A pick on an account that is not among the offers is refused with `AccountNotOffered`, and a pick on a provider whose tier model key is unset is refused with `ModelNotSet` naming the key.
- [x] With a Codex account the owner is not using at `left_pct` 1.0 and a Claude account the owner is using at 12, a pick estimated at 3.0 on Claude is placed; with Codex at 10 the same pick is refused with `OwnerActive` naming the Codex account.
- [x] Two picks, the first on the account in use and the second on the free account, where the second uses up the free account's room, are both placed.
- [x] `is_final` is true for `AccountNotOffered`, `ModelNotSet` and `NeedsYou` and false for `OverBudget` and `OwnerActive`.
- [x] `account_offers` leaves out an agy account while `workers.clis` is `["claude"]`.
- [x] `any_room` is false for no offers and for one offer whose `left_pct` is below its cheapest tier's estimate.
- [x] `quota_summary` of 40 offers is under 4096 bytes, lists 8 accounts and says 32 more exist.
- [x] Verify: `cargo test -p idlemax-core quota::pass`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 9: Stop an account's workers at the 5-hour line and after a rate limit

**Objective:** When an account reaches 60 percent of its 5-hour window, or a worker's turn is ended by a usage-limit or HTTP 429 message, stop that account's workers with their state saved, start none there until the window or the rest allows, and let waiting tasks go to other accounts.
Nothing is retried in a loop.

**Files:**

- Modify: `crates/idlemax-core/src/quota/rules.rs`
- Modify: `crates/idlemax-core/src/quota/rules/tests.rs`
- Modify: `crates/idlemax-core/src/job_run/mod.rs`
- Modify: `crates/idlemax-core/src/worker/run.rs`
- Modify: `crates/idlemax/src/coord/api/agents.rs`
- Modify: `crates/idlemax/src/coord/api/worker_tests.rs`
- Modify: `crates/idlemax/src/coord/api/budget.rs`
- Modify: `crates/idlemax/src/coord/api/budget/tests.rs`
- Modify: `crates/idlemax/src/coord/dispatch.rs`
- Modify: `docs/API.md`
- Modify: `README.md`

**Key Decisions / Notes:**

- The text (A9): Spec 2's worker manager takes `end_text` from the runner seam when a turn ended with no new commit and puts it into the last update of the launch, in `JobUpdate.worker`.
  The coordinator passes that text, and nothing else of a worker's output, to `quota::is_limit_message`.
  The agent classifies nothing, and `JobUpdate.rate_limited` (`crates/idlemax-core/src/protocol.rs:140-141`) stays what old AI jobs use.
- `is_limit_message` (`quota/rules.rs:68-71`) is called by nothing today, so it is free to become the worker classifier: it is true when a line holds one of its own phrases (`rules.rs:19-28`) or passes the job detector's line check, `line_rate_limited` (`job_run/mod.rs:232-236`), which becomes `pub(crate)`.
  That gives it both phrase lists and the check for 429 next to `error`, `status`, `http` or `code` on the same line.
  `job_run::rate_limited` itself is not edited, so old AI jobs are classified as today (A8).
- A turn cut by a limit must not look like a failed check: a turn with no commit is `rejected` by Spec 2's verdict rule.
  So `run::after_job` in `crates/idlemax-core/src/worker/run.rs` gives `shed` for the last update of a `launch` or `resume` job whose `end_text` is a limit message, before any verdict rule.
  The Run keeps its saved state and never becomes `rejected`, so Task 8 never counts it as a failed check.
- The coordinator, in `on_job_update` (`crates/idlemax/src/coord/api/agents.rs:220-261`), which already turns `rate_limited` of an old AI job into `UpdateWrites.limit_hit` and a `Quota` event (`agents.rs:228-241`), written in the update's transaction (`crates/idlemax/src/coord/store/updates.rs:55-57`):
  for a job with a `worker` section whose `end_text` is a limit message it asks `new_rest(store, cfg, live, account, now)` in `coord/api/budget.rs`.
  `new_rest` returns `None` when the account's Budget already has `rests_until_ms`, and otherwise the time `limit_rest_until` gives for a hit now.
  With a new rest it sets `UpdateWrites.limit_hit` to the account, so the hit is stored by the same write as for an old AI job, and it adds one `Quota` event whose text is `rest_event` (Task 5); with none, no hit and no event are written.
  After `commit_update` returned true for a new rest, it calls `dispatch::stop_workers` for that account, the operation behind `POST /v1/workers/stop` with scope `account`.
  A replayed `(device, seq)` makes `commit_update` return false, so it changes nothing and stops nothing.
  For a job without a `worker` section nothing in `on_job_update` changes.
- A hit on an account that already rests stores nothing, logs nothing and stops nothing; that, and the closed Budget, are what "never retries in a loop" rests on.
- The stop site of Spec 2 in `crates/idlemax/src/coord/dispatch.rs` ("when an account with live workers stops being usable, at 60 percent of its 5-hour window or at the cap, `dispatch::stop_workers` stops them") asks `stops_workers(budget.basis)` (Task 5) in place of `account_uses`.
  So the sweep stops an account's live workers while it is `resting`, which is also the safety net when the coordinator died between the commit and the stop, and at `five_hour_stop` (A18).
  It no longer stops them at the cap or on a reading without data: those are `spent` and `no_data` (Open Question 1).
  The stopped runs are `shed`, and `launch_due` relaunches each one when `worker_gate` allows it (Task 6): they resume when the window or the rest allows, like a shed (A18).
- After this task `account_uses` has one caller left, `gather_plan`, which Task 10 changes.
- While `rests_until_ms` is ahead, the Budget has basis `resting`, `offers` leaves the account out, and the next pass places waiting tasks on other accounts, since a task is bound to an account only when a pick names it.
- After the rest a weekly window stays `ResetPending` until a reading taken after the reset arrives (`quota/rules.rs:143-164`), so an account never comes back without fresh data.
- Docs: what a limit in `end_text` does and the two stops in `docs/API.md` next to the line at `docs/API.md:204`, and the "Ban-risk stance" paragraph of `README.md` (`README.md:795-799`) says workers rest the same way.

**Definition of Done:**

- [x] `is_limit_message` is true for `Error 429: Too Many Requests`, `You've hit your limit` and `weekly limit reached`, true for a text of several lines whose last line is `usage limit reached`, and false for `HTTP 200 OK` and `build 4290 passed`; `job_run::rate_limited` still answers false for an AI job whose output is `session limit` and true for `usage limit`.
- [x] `after_job` gives `shed` with no verdict for a last update whose `end_text` is `usage limit reached`, and the verdict of Spec 2's rule for a last update with no commit and the `end_text` `I could not find the cause`.
- [x] One worker job update whose `end_text` is a limit message leaves the hit in `rate_limits` at that time, one `Quota` event with the `rest_event` text, and the account's live runs `shed`; a replay of the same `(device, seq)` changes nothing.
- [x] A second worker's update with such an `end_text` while the account rests leaves `rate_limits.at_ms` unchanged and adds no event.
- [x] An old AI job's update with `rate_limited` still writes the event text `usage limit hit, no more AI jobs this window`.
- [x] A running Run on an account whose Budget turns `five_hour_stop` is stopped and becomes `shed`, and is relaunched once the 5-hour window reads under the line; a running Run on an account whose Budget turns `spent` keeps running.
- [x] With a `shed` run on a resting account, one pick for another task and a second usable account, `launch_due` starts the pick on the second account and leaves the `shed` run where it is.
- [x] `grep -rn "account_uses" crates/idlemax/src/coord/dispatch.rs` prints nothing, and the existing test of `quota/rules/tests.rs` passes with no assertion changed.
- [x] Verify: `cargo test -p idlemax-core quota::rules`, `cargo test -p idlemax-core job_run`, `cargo test -p idlemax-core worker::run`, `cargo test -p idlemax --bin idlemax worker_tests`, `cargo test -p idlemax --bin idlemax coord::api::budget`, `cargo test -p idlemax --bin idlemax coord::dispatch`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 10: Hold Spec 1's plan and orchestrator pass to the Budget and the tier

**Objective:** Make the Budget and the tiers the inputs and the limits of Spec 1's plan and orchestrator pass, in place of the simple rule and of `dispatcher.worker_models`.
The orchestrator is offered only accounts with a usable Budget, is told each task's tier, gets no pass when no account has room, and a task that failed its checks once is tried once more on the standard tier.

**Files:**

- Modify: `crates/idlemax-core/src/dispatch/accounts.rs`
- Modify: `crates/idlemax-core/src/dispatch/prompt.rs`
- Modify: `crates/idlemax-core/src/dispatch/orchestrator.rs`
- Modify: `crates/idlemax-core/src/dispatch/orchestrator/tests.rs`
- Modify: `crates/idlemax-core/src/dispatch/plan.rs`
- Modify: `crates/idlemax-core/src/dispatch/plan/tests.rs`
- Modify: `crates/idlemax-core/src/config/dispatcher.rs`
- Modify: `crates/idlemax-core/src/worker/pace.rs`
- Modify: `crates/idlemax/src/dispatch/mod.rs`
- Modify: `crates/idlemax/src/coord/api/plan.rs`
- Modify: `crates/idlemax/src/coord/store/budget.rs`
- Modify: `README.md`
- Modify: `docs/API.md`
- Test: `crates/idlemax/src/coord/api/plan.rs` (its `tests` module)

**Key Decisions / Notes:**

- The simple rule goes: `account_uses(cfg, views, paused, spent_week_pct)` in `dispatch/accounts.rs` becomes `account_uses(budgets: &[Budget], views: &[QuotaView]) -> Vec<AccountUse>` and loses `FIVE_HOUR_STOP_PCT`, which `quota.five_hour_stop_pct` replaced; `MEASURING_WEEK_CAP_PCT` stays, because `budgets` reads it (Task 6).
  It keeps its name and its return type: `usable` and `why` are the Budget's, and the two used percents come from the views as before.
  So `Plan.accounts`, `Offer.accounts`, `fingerprint` and the "no usable account has the provider `claude`" rule of Spec 1's `pass_due` keep working with no edit of their own, and that rule now holds the pass while its own account is paused, resting, without data, at the 5-hour stop or spent.
- `gather_plan(store, cfg, live, now)` in `crates/idlemax/src/dispatch/mod.rs` calls `budgets` (Task 6) with the views it already reads, then `offers` (Task 5) and `account_offers` (Task 8) with `workers.clis` of the live config, and hands `account_uses` the Budgets; this is the replacement Spec 1 names for its call of the simple rule.
- `dispatcher.worker_models` is replaced by the tier keys, as Spec 1 says it will be: the field and its `validate` clause leave `config/dispatcher.rs`, its row leaves the `README.md` table, and `PlanInput.models` and `Offer.models` take `tier_models` (Task 2) of the live `[quota]`.
- The pass input in `dispatch/prompt.rs`: the accounts part of `summary` becomes `quota_summary(offers)`, and each offered task gains `tier` (`small` or `standard`), which is `tier_for` with the task's failed checks.
  The instructions gain two sentences: prefer an account that is not marked `owner_active` while one has room, and the model follows from the task's tier and the account.
- The pick check at ingest, in `validate` in `dispatch/orchestrator.rs`, where Spec 1 expects `check_picks`: `Offer` gains the account offers, the cost table, the live `[quota]` and the failed checks, and `validate` runs `check_picks` on the picks it accepted.
  A placed pick gets `Placed.model`; a pick refused for good (`is_final`) is dropped and named in `dropped` with the refusal's text; a pick refused with `OverBudget` or `OwnerActive` is kept, because a slot that opens later may take it.
  `ingest_pass` in `coord/api/plan.rs` rebuilds the offer from the current state before it validates, so it builds the account offers and the cost table the way `gather_plan` does.
- The pick check when the plan is built: `gather_plan` runs `check_picks` again on the stored picks whose task is still offered, in the pass's order.
  A placed pick goes to `build` with `Placed.model`, and its reason gains `model set by the tier rule` when `model_changed` is true.
  A refused pick is left out, and `PlanInput` gains `refused: &[(String, String)]`, the task id and the refusal's text, which `build` in `dispatch/plan.rs` uses as the reason of a slot left empty, in the place of its `no model <model> is set` text: `<task title>: <refusal text>`.
  Every pick comes from a pass (A19), so nothing else needs the check, and the start itself is checked once more by `worker_gate` (Task 6).
- When a pass is due: `DueInput` of Spec 1's `pass_due` gains `room`, which is `any_room(offers)`, and `pass_due` refuses with `no account has room for one more Run today` when it is false, checked right after its rule on a usable Claude account.
  A pass that cannot place a worker would only spend Opus quota (D10.9).
- The second attempt of D10.6: `pace::task_open` of Spec 2 is false while a task has a Run in any state except `discarded`, so a `rejected` Run would end the task after one failure.
  It gains one exception: a task whose only Runs that are not `discarded` are one `rejected` Run is open for one more Run; its rule that two Runs ended `rejected` or `discarded` close the task for good stays (Open Question 3).
  The second Run is a new Run on a new branch, which Spec 2's naming already gives, and `check_picks` places it on the standard tier because `failed_checks` is 1.
- "Needs you" after the second failure (D10.6): `failed_checks(store)` in `coord/store/budget.rs` returns, per task id, the number of its Runs whose verdict is `rejected`.
  `step` in `crates/idlemax/src/dispatch/mod.rs`, which already deletes stale rows of the "needs you" store, calls `Store::judge_task(task, digest, "failed its checks twice", now)` for every task with 2 or more, so Spec 1's `discover` takes the task out of the offer and into `Plan.needs_you`.
  `NeedsYou` of `check_picks` stays as the backstop for a pick that is checked before the next `step`.
- Docs: `README.md` gains one paragraph on tiers (which task types start small, the move up after one failed check, "needs you" after the second) and the sentence that the Budget replaced the simple account rule; `docs/API.md` gains `tier` on the tasks of the pass input, its `quota` part, and the refusal texts under `dropped` and in an empty slot's reason.

**Definition of Done:**

- [x] `account_uses` marks an account whose Budget has basis `five_hour_stop` not usable with the Budget's `why`, and `grep -rn "FIVE_HOUR_STOP_PCT" crates` prints nothing.
- [x] `list_settings` no longer lists `dispatcher.worker_models`, and `grep -rn "worker_models" crates README.md` prints nothing.
- [x] `validate` returns a `broken_check` task that was picked with the standard model as a pick on the small model, drops a pick on a provider whose tier model key is unset and names the key in `dropped`, and keeps a pick whose estimate is above the account's `left_pct`.
- [x] `build` with a pick named in `refused` fills no slot from it and gives the empty slot the reason `<task title>: <refusal text>`; the next pick of the pass fills the slot when it is placed.
- [x] `pass_due` refuses with `no account has room for one more Run today` when `room` is false.
- [x] The pass prompt lists each offered task with its `tier`, and its accounts part equals `quota_summary` of the offers.
- [x] `task_open` is true for a task whose only Run is `rejected`, false for a task with a `clean` Run, and false for a task with two `rejected` Runs.
- [x] A coordinator test builds a plan with one pick and its account at the 5-hour stop line: there is no pick in the plan, and the empty slot's reason carries the Budget's `why`.
- [x] A coordinator test with a task that has two `rejected` Runs shows it, after one `step`, under `needs_you` with `failed its checks twice`, and not among the offered tasks.
- [x] Verify: `cargo test -p idlemax-core dispatch`, `cargo test -p idlemax-core worker::pace`, `cargo test -p idlemax-core config`, `cargo test -p idlemax --bin idlemax plan`, `cargo test -p idlemax --bin idlemax coord::dispatch`, `cargo test -p idlemax --test plan`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 11: Show the Budget in `idlemax quota` and `GET /v1/quota`

**Objective:** Let the owner see each account's budget for today, what was spent, what is left, and why it is that number, in `idlemax quota` and in the route behind it.
Nothing about the windows table changes.

**Files:**

- Create: `crates/idlemax/src/cli/out/budget.rs`
- Modify: `crates/idlemax/src/cli/out/mod.rs`
- Modify: `crates/idlemax-core/src/quota.rs`
- Modify: `crates/idlemax/src/coord/api/jobs.rs`
- Modify: `crates/idlemax/src/cli/print.rs`
- Modify: `crates/idlemax/src/cli/handlers.rs`
- Modify: `crates/idlemax/src/cli/home/render.rs`
- Modify: `skills/idlemax/SKILL.md`
- Modify: `docs/API.md`
- Modify: `README.md`
- Test: `crates/idlemax/src/cli/print/tests.rs`
- Test: `crates/idlemax/src/coord/api/budget/tests.rs`

**Key Decisions / Notes:**

- `QuotaView` (`crates/idlemax-core/src/quota.rs:237-260`) gains `budget: Option<Budget>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  It is the account's Budget, repeated on every row of that account, as `spendable` and `reasons` already are; a coordinator older than this omits it, and a client reads that as unknown.
  It sits beside `limited`, which Spec 1 adds to the same struct, and every `QuotaView` literal in the tests already ends in `..Default::default()`, so none needs an edit.
- Only the `quota` handler (`crates/idlemax/src/coord/api/jobs.rs:155-161`) fills it: inside its one `with_store` call it takes `app.live().current()` and calls `budgets` (Task 6) with the views it just built.
  `quota_views` itself stays as it is, because every heartbeat calls it.
- `idlemax quota` prints the windows table as today and under it one budget row per account; `--json` prints the route's body, so it carries `budget` with no extra work (`crates/idlemax/src/cli/handlers.rs:108-123`).

  ```text
  ACCOUNT     BUDGET  SPENT   LEFT    WORKERS  DAY ENDS  WHY
  claude-max  7.3%    2.0%    5.3%    yes      in 5h10m  you use about 6% a day, 3 days to the reset, 22% would expire...
  agy-main    5 runs  3 runs  2 runs  yes      in 9h02m  daily cap: 3 of 5 runs used today
  ```

- `rows.rs` (471 lines) and `out/lists.rs` (402 lines) are over the size line already, so the budget cells, the header and the TOON list live in the new `cli/out/budget.rs`, declared in `cli/out/mod.rs` next to the modules at `mod.rs:12-18`; the TOON view joins the two lists the way `views::status` joins devices and accounts.
- The `SPEND` and `WHY` columns of the windows table stay the verdict for old AI jobs, and the README says so.
- The one-line description of `idlemax quota` in `crates/idlemax/src/cli/home/render.rs:44-46` becomes `see AI account windows, today's budget per account and why`, and `skills/idlemax/SKILL.md` is regenerated with `idlemax skill`, run in a temp home built with the recipe in `AGENTS.md`.
- `docs/API.md` gains a section "The budget of each AI account" with this body (example data), the meaning of each field, each `basis` code and each cost `basis`, the account day, and the note that account days are kept 30 days in `quota_days` with the budget each day had:

  ```json
  {"account": "claude-max", "provider": "claude", "unit": "weekly_pct",
   "day_start_ms": 1730000000000, "day_end_ms": 1730086400000,
   "spendable_pct": 7.3, "spent_pct": 2.0, "left_pct": 5.3, "usable": true,
   "rests_until_ms": null, "owner_active": false, "cap_runs": null, "runs_today": 4,
   "costs": [{"tier": "small", "model": "haiku", "est_pct": 0.4, "basis": "same_model",
              "samples": 9, "five_hour_pct": 2.1},
             {"tier": "standard", "model": "sonnet", "est_pct": 8.0, "basis": "seed",
              "samples": 2, "five_hour_pct": null}],
   "basis": "learned",
   "why": "you use about 6% a day, 3 days to the reset, 22% would expire unused: today's share is 7.3%"}
  ```

- `README.md`: the "AI quota maximizer" section shows the new table and explains the budget in three sentences.
- No manual run with an AI account proves this task: `AGENTS.md` forbids configuring one in a dev run, so the route and the rendering are proven by tests on an in-memory store and on fixed values.

**Definition of Done:**

- [x] `GET /v1/quota` on a store with one account returns rows that each carry the same `budget` object, with `spendable_pct`, `spent_pct`, `why` and one `costs` line per tier with `est_pct`, `basis` and `samples`.
- [x] A row without `budget` deserializes, and `idlemax quota` then prints the windows table alone.
- [x] The budget table prints percent for a `weekly_pct` account and runs for a `daily_cap_runs` account, and a closed account shows `no` under `WORKERS` with its `why`.
- [x] `idlemax quota --json` output contains `"budget"`.
- [x] `skills/idlemax/SKILL.md` matches what `idlemax skill` prints, which the `skill` integration test checks.
- [x] Verify: `cargo test -p idlemax --bin idlemax quota`, `cargo test -p idlemax --bin idlemax coord::api::budget`, `cargo test -p idlemax --test skill`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`

### Task 12: Needs the owner's OK: run the measuring week and tune the numbers

**Objective:** Set the seed costs from Task 1 before the week starts, and after seven account days of the dispatcher in `auto` mode on the owner's Mac check the three facts of roadmap item 35 against what was recorded and tune the numbers of D10.
It reads the real state and changes real settings, so the owner does it or approves each step.

**Files:**

- Modify: `docs/QUOTA-COSTS.md`
- Modify: `docs/ROADMAP.md`

**Key Decisions / Notes:**

- Preconditions: Tasks 1 to 11 and Spec 2 are installed on the Mac by the owner, the owner switched the dispatcher to `auto`, and the owner has answered the fence question or accepted Spec 2's soft fences for the Mac (Open Question 2).
- Before the week starts the owner sets `quota.seed_small_pct` and `quota.seed_standard_pct` to the weekly points per run that Task 1 measured for `haiku` and `sonnet`, with `idlemax config set`; the change applies with no restart (A2).
- The real database is read only through read-only access, `sqlite3 'file:<path to coord.db>?mode=ro'`, and through `idlemax quota --json` and `idlemax events` of the owner's installed build; no dev build is pointed at the real home.
- Fact 1, the budget comes from measurements: every claude and codex account has seven closed rows with an `owner_pct` in `quota_days`, and every `costs` line of a model that ran in the week has 5 or more `samples` and a basis other than `seed` in `idlemax quota --json` (D10.1, D10.2).
- Fact 2, a day's spend never passed that day's budget: for every closed row of `quota_days` in the week (`SELECT account, day_start_ms, spendable_pct, idlemax_pct FROM quota_days WHERE closed = 1`), `idlemax_pct` is at or under `spendable_pct` plus the estimated cost of the Runs that were running when the budget ran out (D10.3).
- Fact 3, no limit that stopped the owner was caused by a worker: `idlemax events` shows no `quota` event for the week, or each one is explained in the file (D10.4, D10.7).
- Tuning: the two seeds, `quota.safety_margin_pct`, `quota.five_hour_stop_pct` and `quota.owner_reserve_pct` are compared with what the week showed, and a changed value is set with `idlemax config set` by the owner and written into `docs/QUOTA-COSTS.md` with the reason (D10, last paragraph).
- `docs/ROADMAP.md`: item 35 is marked done with the date and the commit that closed it, or the fact that failed is written as what is left.
  Item 11 records what is done (workers use measured costs, and worker sessions are left out of the interactive-use check) and what is left: old AI jobs keep their fixed figures and the `SHORTCUT` (A8), and an old AI job's own session still counts as interactive use.

**Definition of Done:**

- [ ] `docs/QUOTA-COSTS.md` states the two seed values set before the week and the points per run they came from.
- [ ] `docs/QUOTA-COSTS.md` has a section for the week with the three facts, each with the figures that prove it or the day and account where it failed.
- [ ] Every setting that was changed is listed with its old value, its new value and the reason.
- [ ] Roadmap items 11 and 35 say what is done and what is left, and item 11 is not marked done while its `SHORTCUT` stays.
- [ ] Nothing under the idlemax home was written by anything but the owner's installed build.
- [ ] Verify: `git status --short` lists only `docs/QUOTA-COSTS.md` and `docs/ROADMAP.md`; the task adds no code and no test, so `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass as before.

## Open Questions

1. **When a Budget closes for a reason other than a rest or the 5-hour line, do the account's running workers stop?**
   D10.3 says that when today's budget is gone "the account rests until tomorrow", A18 stops running workers at the 5-hour line, and Spec 2's simple rule also stops them at the 10 percent cap and on a reading without data (Spec 2, Task 9).
   A running Run holds its own estimate in `committed_pct` (Task 5), so a Run that fills the day would close the account and be stopped by its own start, in a loop; and when a reading goes stale or waits for the weekly reset, the running workers' own sessions are what writes the next reading.
   Recommended, and what Tasks 5 and 9 build: at `spent` and at `no_data` no worker starts and the running ones finish their task, so the overshoot is at most the cost of the Runs already running (Truth 1); only `resting` and `five_hour_stop` stop running workers.
2. **How strong is the worker fence (A13)?**
   The owner's answer is pending, and it touches this plan in two places: Task 1 runs Spec 2's launch form by hand, which bypasses permission prompts for Claude, and Task 12 needs a week of `auto` mode on the Mac under whatever fence exists then.
   Recommended: Task 1 runs only inside the throwaway repository with the owner watching, which needs no stronger fence, and Task 12 starts only after the owner has answered or has accepted Spec 2's soft fences for the Mac, with that choice written into `docs/QUOTA-COSTS.md`.
3. **Is the second attempt of D10.6 automatic?**
   D10.6 moves a task up a tier after one failed check and sends it to "needs you" after the second, which needs a second Run of the same task.
   Spec 2's `pace::task_open` is false while a task has a Run in any state except `discarded` (Spec 2, Task 9), so after one `rejected` Run the task waits until the owner discards that Run, which is "needs you" after the first failure.
   Recommended, and what Task 10 builds: one `rejected` Run leaves the task open for one more Run, on the standard tier and on a new branch, and Spec 2's rule that two failed Runs close the task for good stays.

## Review Disposition

1. fixed: Task 3 reads the after reading from `quota_snapshots`, and Task 1 item 10 records when the capture is written.
2. fixed differently: A7 gives the start and the end of the measuring week to Spec 2, so Task 6 reads `pace::measuring_week_end` and `pace::week_spend` and defines no end of its own.
3. fixed: Task 10 names every consumer of `AccountUse`, removes `dispatcher.worker_models`, holds the pass through its account's Budget and uses `any_room` and `quota_summary`, which clash with no Spec 1 name; A19 leaves no fallback picker to wire.
4. fixed: Task 6 swaps the launch, the relaunch and the reviewer site, Task 9 the stop site, which asks `stops_workers` and so never stops a Run for `spent`; A18 keeps the stop at the 5-hour line.
5. fixed differently: A9 gives the read to Spec 2's runner seam, which sends `end_text`; Task 9 classifies it on the coordinator and maps a Run ended by a limit to `shed`, so it is never a failed check.
6. fixed differently: Spec 2 now sends the text and not `rate_limited`, so the two paths cannot collide; Task 9 decides with `new_rest` before the commit, writes the hit through the existing `UpdateWrites.limit_hit`, and has no separate `on_limit_hit`.
7. fixed: `limit_block` and `job_run::rate_limited` are not edited; `limit_rest_until` is a worker-only function in `quota/spend.rs` (Task 5), and `is_limit_message`, which nothing called, is the worker classifier (Task 9).
8. fixed: Task 2 edits the one literal in `quota/rules/tests.rs`, and Truth 3 speaks of assertions.
9. fixed: `offers` only orders (Task 5), `check_picks` refuses an account in use only while a free one has room (Task 8), and no `left_pct` is compared across units.
10. fixed: `quota_days` holds each day's `spendable_pct`, `basis` and `why` (Tasks 4 and 6), `budget_days` reads them, and `Budget.costs` shows each tier's estimate and basis (Tasks 3, 5, 11, 12).
11. fixed: `check_picks` sets the model (Task 8), the pass input carries each task's `tier`, and a task that failed twice is written to Spec 1's "needs you" store with `Store::judge_task` (Task 10).
12. fixed: `gate` takes the Run's estimate (Task 5), and `worker_gate` is asked at every launch and relaunch (Task 6).
13. fixed: there is no `SpendKind`; `gate` is the worker gate and the ten-row table is documentation (Tasks 5 and 6).
14. fixed: Task 7 walks past worker files up to a bound that fails closed, and its notes and Task 12 say what is left of roadmap item 11.
15. fixed differently: the seed is for the weekly window only and is two settings, `quota.seed_small_pct` and `quota.seed_standard_pct`; the 5-hour mean is reported and never seeded, because nothing gates on it; `token_class_pct` keeps its numbers (A8).
16. fixed: Task 1 uses Spec 2's `agent_args`, its DoD names the capture file, and the count sentence says the runs are outside the measuring week's sum.
17. fixed: Spec 2 now skips a run or a pick that may not launch, and Tasks 6 and 9 each have the coordinator test.
18. fixed: every Files list names the files its notes change, and Specs 1 and 2 are named by path and symbol.
19. fixed differently: the old Task 9 is split in four to keep 12 tasks: the pure part (Task 8), the Spec 1 wiring (Task 10), and the Spec 2 wiring by start (Task 6) and by stop (Task 9); each Verify line runs the `dispatch`, `worker` and `coord::dispatch` tests it touches.
20. fixed: `account_offers` leaves out a CLI that is not in `workers.clis` (Task 8), and `runs_started_in` counts only Runs with a session id (Tasks 4 and 6).
21. fixed: `QuotaConfig` moves to `quota/config.rs` (Task 2), and the store tests live in `coord/store/budget/tests.rs` (Tasks 3 and 4).
22. fixed: `SAME_RESET_MS` moves to `quota.rs` (Task 3).
23. fixed: `small_task_types` is checked against `TaskSource` (Task 2).
24. fixed: `budgets` takes the views (Task 6).
25. fixed: the DoD asserts that `rate_limits.at_ms` is unchanged (Task 9).
26. fixed: `validate` refuses a Claude tier model whose name contains `opus` (Task 2).
