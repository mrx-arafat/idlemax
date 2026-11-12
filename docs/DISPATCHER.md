# The dispatcher: design decisions

Agreed with the owner on 2026-11-11, in a design session of 21 questions.
This file is the source of truth for the five dispatcher specs in `docs/plans/` and for roadmap items 33 to 37.
A spec builds what is decided here and does not decide it again.
When the code makes a decision impossible, or two decisions collide, the spec lists it under Open Questions and this file changes first.

## What the dispatcher is

Today idlemax runs queued commands on spare capacity and spends leftover AI quota near a reset.
The dispatcher makes AI coding workers the main kind of work.
On every device the owner switches on, idlemax measures the real room (memory, disk, quota).
An orchestrator fills that room with workers on leftover tasks from projects the owner switched on.
Each worker runs in its own git worktree and its own terminal pane, commits locally only, and is checked before the owner hears about it.
The owner can stop everything in seconds and resume later from the same point.
Plain command jobs (builds, tests, backups) stay as the second kind of work.

## Words

| Word | Meaning |
|---|---|
| Owner | The person whose machines and quota these are. |
| Owner session | An agent session or app the owner started. idlemax never stops one, edits its files or shares its folder. |
| Device | A machine that runs an idlemax agent. |
| Project | A git repository on a device that idlemax knows about. Off until the owner switches it on. |
| Task | One small piece of work in a project, with a check that proves it is done. |
| Worker | One coding agent process that idlemax started for one task. |
| Orchestrator | The Claude Opus pass that picks project, task, provider and model for each open slot and judges scope. |
| Brief | The written instruction a worker starts with. |
| Run | The record of one worker on one task, from start to verdict. |
| Verdict | `clean`, `needs_you` or `rejected`. |
| Lines | Hold, shed and emergency: the three resource thresholds. |
| Budget | The quota idlemax may spend on one account today. |
| Surplus | The quota that would expire unused at reset, given the owner's usual use. |
| Tier | The model class of a worker: `small` (Haiku class) or `standard` (Sonnet class). |
| Profile | `laptop`, `workbox` or `production`. |

## Measured on the owner's Mac (2026-11-11)

The machine has 16 GiB of memory and 10 cores.

- Memory used was 80 to 82 percent (app, wired and compressed memory over the total, the figure Activity Monitor shows).
- At that reading 8.3 GiB sat in swap and 26 GiB of pages were compressed into 5.8 GiB, so used percent alone hides the overflow.
- Swap-in was 0 to 5 pages per second: the machine was overcommitted but not thrashing.
- Free disk was 3 to 5.5 GiB, and swap grows on that disk.
- The macOS memory pressure level was `Warn` the whole time.
  The guard pauses on `Warn`, so it held all work while there was room (roadmap item 30).
- A Claude Code worker process takes 0.3 to 0.4 GiB.
  The owner's personal MCP servers add several helper processes of 0.3 to 0.9 GiB to each session.
- The agent CLIs `claude`, `codex`, `opencode`, `agy` and `gemini` are installed.
  Herdr 0.9.4 can start each of them with `herdr agent start <name> --kind <kind> --pane <id>`.
- A session can be continued with `claude --resume <id>`, `codex resume`, `opencode --session <id>` and `agy --conversation <id>`, and each CLI takes `--model`.
  Only the Claude form was seen working; the others come from `--help` and the spec that uses them must prove them.
- `herdr agent list` returns for each agent its kind, its status (`idle`, `working`, `blocked`, `done`, `unknown`), its folder, its pane, tab and workspace, and the agent's session id.
- `claude auth status --json` reports the logged-in account (`orgId`, `subscriptionType`, `planTier` and more) without showing a token.
  `codex login status` only says that a login exists.
- idlemax reads quota today for Claude (the statusline capture) and Codex (session files), as account-level 5-hour and weekly used percent.
  It reads no quota for agy or OpenCode, and no per-model figure for any provider.
- The owner's Herdr has a plugin that opens an agent tab in every worktree that Herdr itself creates.
- Two Linux work servers are saved as Herdr machines, each with its own Herdr session.

## Decisions

### D1. Purpose

idlemax keeps two things from sitting idle: the spare memory and CPU of each device, and each AI provider's weekly quota.
It does that by putting AI coding workers on the leftover tasks of the owner's own projects, without getting in the owner's way.
The main score of every worker is that it did the right, focused task and broke nothing that worked.
A worker runs focused tests only, never a whole suite, and adds no test the task does not need.

### D2. Who decides what

- **idlemax rules** (fixed code, no AI): whether a worker may start, how many, and when one must stop.
  Memory, disk and quota limits live here and no AI can override them.
- **The orchestrator** (Claude Opus, always): which project, which task, and which provider and model fill each open slot.
  It also judges whether a worker stayed on its task.
- **The owner**: sets the fence (which projects and devices are on, the quota share) and can stop everything at any moment.

The orchestrator may use any agent CLI that already exists on the device as a worker.
idlemax carries a written rule set that holds everything the orchestrator needs to decide the next thing to do (see D18).

### D3. Approval and transparency

Inside the fence idlemax launches workers by itself, with no approval per launch.
It starts in `propose` mode, where it only shows what it would launch, and the owner flips it to `auto`.
The owner can always see what was launched, why, and what each worker is doing now.

### D4. When workers run

Workers run while the owner is working at the device, not only while the owner is away.
The rule of waiting for user idle time does not apply to AI workers.
Owner sessions always win: under pressure idlemax stops its own newest worker first and never touches an owner session.
A worker never opens in a tab the owner is using.

### D5. Which projects

Every project is off by default.
idlemax may work on a project only after the owner switches it on, and switching it off takes it out of the pool.
Among switched-on projects, idlemax prefers the ones the owner is not active in.
It uses the owner's active project only when no other project has a ready task, and then only a task in files the owner has not touched.
The owner is active in a project when one of the owner's agent sessions there is working or was working recently.

### D6. What a worker may do

A worker gets its own git worktree and branch and never works in the owner's folder.
It may edit, run focused tests and commit locally.
It may not push, open or merge pull requests, deploy, touch databases, send messages or spend money.
Nothing leaves the machine without the owner's permission.
After its commit a worker may take another task in that project (see D9).
When a task is finished idlemax tells the owner what was done; the owner reviews whenever they choose.

### D7. Which tasks count as ready

For a switched-on project the orchestrator looks for work in this order:

1. Things already broken: failing focused tests, lint or typecheck on the base branch.
2. Items in the project's roadmap file that say when they are done.
3. Open GitHub issues assigned to the owner.
4. Unfinished work flagged in the owner's recent agent sessions (failing tests, an interrupted turn).

A task is ready only if it is small enough for one worker in one sitting, has a clear check that proves it is done, and needs no decision from the owner.
Anything vague, architectural, or needing credentials or outside access is not attempted.
It goes on a "needs you" list with the reason.

### D8. How a finished task is checked

A worker saying "done" counts for nothing.
The worker is trusted to do the work because it gets a clear brief, and idlemax verifies the result itself before it notifies the owner.

- **The brief** states the goal, the files in scope, what not to touch, the exact check that proves the task is done, and "focused tests only".
- **Baseline check** (every task): the project's focused checks are recorded on the base branch before the worker starts and must still pass on the worker's branch.
- **Scope check** (every task): the diff touches only what the task needs. Extra refactors, extra tests and unrelated files count against it.
- **Independent review** (only when risk is high): an agent from a different provider than the author reads the diff against the task.
  Risk is high for a large diff, for shared or core code, and when the checks cannot cover the change.

Each task gets one verdict: `clean`, `needs_you` (with the exact concern) or `rejected` (the branch is kept but the task is never reported as done).

### D9. How much unreviewed work may pile up

Per project at most 3 finished tasks may wait for the owner's review.
Then idlemax moves its workers to another switched-on project, or stops.
Each task is its own branch off the base branch, not stacked on an unreviewed one, unless the task truly depends on the previous one.

### D10. Quota: measure, then spend the surplus

The owner needs the weekly quota for their own work, and that always comes first.

1. **Measure first.** Every run records provider, account, model, task type and the quota used before and after, on the 5-hour and the weekly window.
   That builds a real cost table per model.
   The first week is a measuring week in which idlemax may spend at most 10 percent of an account's weekly quota.
2. **The budget is the owner's surplus.** idlemax learns the owner's daily use per account and spends only what would expire unused at reset, minus a safety margin.
   Until it has a week of the owner's numbers it reserves 50 percent of the weekly quota for the owner.
3. **Paced by day.** Each account gets a daily budget of the spendable weekly quota left, divided by the days until reset.
   When today's budget is gone the account rests until tomorrow and work moves to another account.
4. **Never lock the owner out.** idlemax stops using an account once its 5-hour window is 60 percent used, and prefers providers the owner is not using at that moment.
5. **Last day.** On the day before reset idlemax may spend what is left, down to a 15 percent floor.
6. **Model tiers.** Opus orchestrates only.
   Workers default to the standard tier (Sonnet class) and small mechanical tasks go to the small tier (Haiku class).
   A task moves up a tier only after the cheaper model failed its checks once; after a second failure it goes on the "needs you" list.
7. **Rate limits.** On a usage-limit or HTTP 429 message idlemax stops that account's workers, saves their state, rests the account until its reset time and moves waiting tasks to another account.
   It never retries in a loop.
8. **Providers idlemax cannot read.** agy and OpenCode run on a fixed daily cap the owner sets, until a quota reader exists.
9. **Token efficiency.** Workers are lean: they load the project's own instructions but not the owner's personal MCP servers, and the brief names the files in scope.
   One task gets one fresh session.
   The orchestrator runs as a short pass over a compact state summary each time a slot opens, never as an always-open session.

How each plan counts usage per model is not known from documents.
The first task of the quota spec is to measure it on the owner's accounts.
The numbers in this section are starting values that the measurements tune.

### D11. Stop and resume

- **Stop:** `idlemax stop`, also a key in the TUI and a switch in the dashboard.
  Within seconds every worker's current turn is cut, each agent exits so its memory is freed, its pane closes, and nothing new starts.
  Uncommitted edits stay in the worktree.
- **Saved per worker:** project, task, branch, worktree, provider, account, model and session id.
- **Resume:** only when the owner says `idlemax start`.
  Each worker reopens in its pane and continues the same session, after memory and quota are checked again.
- A stop survives a reboot, and idlemax never resumes an owner stop by itself.

The existing `pause`, `resume` and `stop-all` commands and the `$IDLEMAX_HOME/PAUSE` kill switch keep working and apply to workers too.

### D12. Control surfaces

One switch, reachable from four places that all call the same coordinator API:

1. **Browser dashboard:** a main on/off switch, a toggle per project and per device, and the live worker list.
2. **TUI:** the same controls.
3. **CLI:** `idlemax start`, `idlemax stop`, `idlemax project on|off <name>`.
4. **AI agents:** an MCP server built into idlemax (`idlemax mcp`), so an agent the owner instructs can read status and flip the switch.
   Its tools are status, start, stop, project on/off and list finished work.

Only the owner's own sessions get these controls.
A worker that idlemax launched can never start idlemax, switch a project on or raise a limit.

### D13. How many workers

There is no fixed worker count.
idlemax deploys as many workers as memory, disk and the daily budget allow.
It adds one worker at a time and lets memory settle before it adds the next.

### D14. The lines, and who gets stopped

On macOS the readings are memory used percent, the swap-in rate and free disk.

1. **Hold:** at 90 percent memory used, or when swap-in has not been low for the last minute, or below 4 GiB of free disk, no new worker starts. Running workers continue.
2. **Shed:** at 95 percent memory used, or when swap-in stays high, or below 2 GiB of free disk, idlemax stops one worker at a time until memory is back under 90 percent.
   The order is the most recently started first, then the lowest-priority task, then the biggest memory user.
   A shed worker's task goes back in the queue and restarts by itself when there is room again.
3. **Emergency:** at 98 percent memory used, or below 1 GiB of free disk, every idlemax worker stops at once, with state saved.

idlemax frees only what it owns: its workers and the helper processes they started.
It never closes the owner's apps or sessions; it tells the owner the biggest memory users.
The numbers are starting values for the `laptop` profile and are settings, not constants.

### D15. Where workers appear in Herdr

On a device with Herdr, workers appear in one dedicated workspace named `idlemax workers`.
It holds one tab per project, named `im: <Project>`, and one pane per worker, named `im-<project>-<task>` within Herdr's name rule `[a-z][a-z0-9_-]{0,31}`.
The owner's project workspaces stay exactly as they were.

A Herdr workspace is only a view; the code lives in git.

- The worker's folder is a git worktree of the project's own repository, so its branch is visible from the owner's checkout with no syncing.
- Branches are named `idlemax/<task>`.
- idlemax keeps its worktrees in one place of its own, `$IDLEMAX_HOME/worktrees/<project>/<task>`, apart from the owner's worktrees.
- idlemax creates worktrees with plain git and not through Herdr, because of the plugin noted under the measurements.
- A fresh worktree has no installed dependencies; the project's setup command (D18) installs them, and the disk lines of D14 cover the cost.

### D16. How the owner takes the work

`idlemax review` lists finished tasks per project with the verdict, a one-line summary, the diff size and the branch.

- **Keep:** the task is marked accepted, the branch stays in the repository, and the worktree folder is removed.
  With `--merge` idlemax merges the branch into the owner's current branch, only when the owner's working tree is clean and there is no conflict; otherwise it stops and says why.
- **Discard:** the branch and the worktree are deleted, after one confirmation.

idlemax never merges into the owner's branches unless asked for that task.
Worktrees are removed with git's own safe removal, without force, so git refuses when uncommitted work is left.

### D17. What idlemax tells the owner

Everything is visible when the owner looks, and only three things interrupt.

**Notifications** (a desktop notification and an inbox entry):

1. A task finished: project, one line on what was done, and the verdict.
   Tasks that finish within 10 minutes of each other are combined into one message.
2. Something needs the owner: a task idlemax could not do alone, an account out of quota for the week, an emergency stop.
3. One daily summary: tasks done, quota spent per account against the budget, and what waits for review.

**Silent but visible** (dashboard, TUI, `idlemax status`, the `idlemax workers` workspace): a worker starting, a move to another provider, a hold, a shed, and why each task was picked.

In the dashboard and the TUI every worker and task carries a state label, a one-line summary, and the quota spent against the budget for today and for previous days.

### D18. Where the rules live

1. **Hard limits** are enforced by code and stored in configuration: the lines, the owner's quota share, which projects and devices are on, the review cap.
   They change only through the dashboard, the TUI, `idlemax config`, or an agent the owner instructs.
2. **Judgment rules** are one text file, `$IDLEMAX_HOME/rules.md`, shipped with good defaults.
   It says how to pick a task, what the brief contains, focused tests only, stay in scope, never break existing behaviour, and what never to touch.
3. **Per-project settings** live in idlemax's own configuration, so nothing is added to the owner's repositories: base branch, setup command, focused check command, off-limits paths and priority.
   Workers still read each project's own `AGENTS.md` or `CLAUDE.md`.
4. **Learning:** after a rejected or discarded task the orchestrator may propose a new line for `rules.md`.
   It takes effect only after the owner approves it.

### D19. Devices and accounts

- **One agent per device, one coordinator for all.**
  idlemax is installed on each server; servers are not driven from the laptop, because the laptop sleeps and the lines must act locally within seconds.
  Each agent measures its own resources and drives its own local Herdr.
  The coordinator does the picking and the pacing for all devices and moves to an always-on server that is reachable only over the owner's private network.
- **Quota belongs to the account, not the device.**
  Two devices on the same login share one budget, and different logins have separate budgets.
  idlemax detects the Claude account by itself from `claude auth status --json`, and never reads a login token.
  For Codex the owner names the account once per device, and idlemax warns when two differently named accounts show the same reset time and usage.
- **Three profiles.**
  `laptop`: the owner comes first, with the rules above.
  `workbox`: a server with nobody at the keyboard and nothing else to protect; it works around the clock and uses almost all of its memory and CPU, keeping a small reserve for the system.
  `production`: a server that runs real services; the services come first and idlemax gets a small capped share.
  A server idlemax knows nothing about starts as `production`.
- **Right signals per system.**
  Linux uses memory pressure stall information (PSI), available memory and the cgroup limit.
  The same three lines apply on every system, with numbers per profile.
- **Every device is off by default**, like projects, and the owner chooses which projects are on for each device.
- **A server-only account works all week.**
  When an account is used only by servers, the owner's reserved share drops to a small margin and the rest is spread evenly over the seven days.
- **Out of quota does not mean idle.**
  When a server's daily budget is spent it runs work that costs no quota: each switched-on project's checks, to find what is broken, which becomes the next day's task list.
- **Same view everywhere.**
  Worker panes on a server appear in that server's Herdr session under the same `idlemax workers` workspace, and the dashboard shows all devices together.
  A server without Herdr runs workers headless, with the same brief, checks, stop and resume.
- Agent CLIs must be logged in on each server; the setup check reports a missing login as "needs you".

### D20. The owner's two servers

Both saved Linux servers are work machines with no live services for customers, so both get the `workbox` profile.

### D21. Build order

Five steps, each its own spec, each usable when it lands.

1. **Decide without acting, on the Mac** (roadmap item 33).
2. **Launch, verify, stop** (item 34).
3. **The quota brain** (item 35).
4. **Control and visibility** (item 36).
5. **Servers** (item 37).

### D22. Worker limits are enforced, not only instructed

A worker starts with no push access, no deploy or database credentials and none of the owner's personal MCP servers.
D6 then holds even when a worker ignores its brief.

## Spec map

Each decision has one owning spec.
A spec that needs something another spec owns uses it by the shared name below and does not build it.

| Spec | File | Owns |
|---|---|---|
| 1. Plan | `docs/plans/2026-11-11-dispatcher-1-plan.md` | D1, D2, D3 (`propose` mode), D4 (no idle wait for workers), D5, D7, D13 (the count), D14 (the hold decision on macOS), D18 items 2 and 3 |
| 2. Workers | `docs/plans/2026-11-11-dispatcher-2-workers.md` | D3 (`auto` mode), D6, D8, D9, D11, D13 (one at a time), D14 (shed and emergency), D15, D16, D22, the run record of D10 item 1 and its 10 percent cap |
| 3. Quota | `docs/plans/2026-11-11-dispatcher-3-quota.md` | D10 items 1 (the cost table) to 9, except the run record; of item 9 only the quota side (Spec 2 builds the lean launch, Spec 1 the short pass) |
| 4. Control | `docs/plans/2026-11-11-dispatcher-4-control.md` | D12 (dashboard, TUI, MCP), D17, D18 items 1 and 4 |
| 5. Servers | `docs/plans/2026-11-11-dispatcher-5-servers.md` | D19, D20 |
| 6. Sandbox | `docs/plans/2026-11-13-dispatcher-6-sandbox.md` | A13 (the worker sandbox), the OS layer under D6 and D22 |

## Shared names

These names are fixed so that five specs written apart still fit together.
A spec that finds a name impossible lists it under Open Questions and does not rename it alone.

**Commands**

| Command | Owner |
|---|---|
| `idlemax plan` (what the dispatcher would launch now, and why) | Spec 1 |
| `idlemax project list`, `idlemax project on <name>`, `idlemax project off <name>` | Spec 1 |
| `idlemax start`, `idlemax stop` | Spec 2 |
| `idlemax review`, `idlemax review keep <run> [--merge]`, `idlemax review discard <run>` | Spec 2 |
| `idlemax mcp` | Spec 4 |
| `idlemax device on <id>`, `idlemax device off <id>` | Spec 5 |

**Data**

| Name | Fields that other specs rely on | Owner |
|---|---|---|
| Project | name, device, path, enabled, base branch, setup command, focused check command, off-limits paths, priority | Spec 1 |
| Task | id, project, source (`broken_check`, `roadmap`, `github_issue`, `session`), title, done check, ready or the "needs you" reason | Spec 1 |
| Plan | per device: the line state (run, hold, shed, emergency) with its reason, open slots, and per slot the project, task, provider, account, model and the reason for the pick | Spec 1 |
| Run | id, device, project, task, branch, worktree, provider, account, model, session id, state, start and end time, quota before and after per window, verdict and its concern | Spec 2 |
| Run states | `starting`, `running`, `checking`, `clean`, `needs_you`, `rejected`, `stopped` (owner stop), `shed` (back in the queue), `kept`, `discarded` | Spec 2 |
| Budget | per account and day: the spendable percent, the percent spent, and why it is that number | Spec 3 |
| Tier | `small`, `standard` | Spec 3 |
| Profile | `laptop`, `workbox`, `production` | Spec 5 |

**Dispatcher modes:** `off`, `propose`, `auto`.

Until Spec 3 lands, Specs 1 and 2 use one simple rule in place of the Budget: an account may be used only while its measuring-week spend is under 10 percent of the weekly quota and its 5-hour window is under 60 percent used.
Until Spec 5 lands, there is one device, the Mac, with the `laptop` profile.

## Answers to the specs' open questions (2026-11-12)

The five specs raised 60 open questions and an independent review of each spec found where they disagree.
The owner handed these choices to the dispatcher's author, except the strength of the worker fence (A13).
Each answer names the spec that builds it, and it overrides a spec's own recommendation where the two differ.

### Wiring between specs

- **A1. The mode.** `dispatcher.mode` lives in `config.toml`. Spec 1 builds `off` and `propose`, the route `POST /v1/dispatcher/mode` and the command `idlemax mode <off|propose|auto>`. Spec 2 adds `auto`.
- **A2. Settings take effect without a restart.** Each daemon re-reads the dispatcher keys it uses (`[dispatcher]`, `[workers]` and the new quota keys) when the mtime of `config.toml` changes. Every route that changes a setting writes the file through the existing config editor. This one rule serves Spec 1's mode, Spec 4's `POST /v1/limits` and every later key.
- **A3. Every switch is a coordinator route**, with on and off as separate calls: the mode, project on and off, device on and off, start, stop. The dashboard and `idlemax mcp` reach nothing else.
- **A4. Task and Project fields.** Spec 1 adds `scope_paths` to Task (named by the orchestrator when it picks the task) and `core_paths` to Project (the owner's list for the risk rule of D8). Setup and check commands are argv, never a shell line, in every spec.
- **A5. Run fields.** Spec 2 adds `reason` (copied from the Plan slot at launch) to Run, so every surface can say why a task was picked.
- **A6. The line state.** Spec 1's pure function computes it on the agent, which samples every 5 s while a worker runs. Spec 2 keeps the shed going until memory is under the hold line, as D14 says.
- **A7. The measuring week** belongs to Spec 2: it starts at an account's first Run, ends seven days later, and caps that account at 10 percent of each weekly window until Spec 3 lands. Spec 3's Budget takes the smaller of the two figures.
- **A8. Worker jobs and the old AI rule.** Worker jobs keep `cost = ai` and `ai_account`, are marked as worker jobs, and are exempt from the one-AI-lease rule in both directions. Only the Budget limits an account. Old AI jobs keep their rules and fixed cost figures unchanged.
- **A9. The limit message.** Spec 2's runner seam reports the text that ended a worker's turn with an error. Spec 3 classifies it and rests the account. Nothing else of a worker's output is classified.
- **A10. Owner actions are never held.** `idlemax review keep` and `discard`, `stop`, and every switch work whatever the line state, the guard or a pause says.
- **A11. One check-job mechanism.** Spec 1's check job is the only one. Spec 5 reuses it when a budget is spent. `device off` stops dispatcher-created jobs too, not only workers.
- **A12. The worker marker.** The marker is the existing `IDLEMAX_JOB_ID`, which Spec 2 passes into the pane. No second marker is added.

### Safety

- **A13. Worker fence strength: the owner's answer to Q22 is pending.** Until it lands, Spec 2 builds the soft fences and lists the sandbox as an Open Question.
- **A13 answered (2026-11-13).** Decided by the orchestrator under the owner's delegation: workers run inside an OS sandbox by default where one is available, and the soft fences stay as a second layer. On Linux the sandbox is bubblewrap; on macOS it is `sandbox-exec` with a generated Seatbelt profile. A sandboxed worker writes only its worktree, the git objects and refs its commits need and a temp dir, reads the project and the toolchains but not `~/.ssh`, `~/.config/gh`, `~/.git-credentials`, the idlemax state dir or `~/.claude` beyond what its CLI needs to authenticate, and reaches the network only through an allow-list proxy that opens the model API hosts. `workers.sandbox` takes `auto` (the default: sandbox when available, soft fences with a warning when not), `required` and `off`; a `production` device always requires it. The Herdr and headless hosts both wrap the agent CLI, and `idlemax doctor` reports whether the sandbox is available. Spec 6 builds it.
- **A14. Workers call no idlemax route.** A worker gets no idlemax token, so it cannot start, stop or switch anything; specs drop any claim that a worker can stop idlemax.
- **A15. A credential per device.** Spec 5 gives each device's agent its own credential that reaches only the `/v1/agent/*` routes. No server runs in `auto` before that credential and the answer to Q22 exist; until then servers stay in `propose`.
- **A16. Dispatcher work follows the lines, not the idle rule.** The orchestrator pass, the base checkout, setup and check commands, and workers are admitted by the lines of D14, not by user idle time or the pause on memory pressure `Warn`. They keep the runner's OS throttles. Plain jobs the owner queues keep the guard.
- **A17. Battery and heat.** Dispatcher work holds when the guard would hold for a battery under its floor or for thermal throttling.
- **A18. The 5-hour stop.** At 60 percent of an account's 5-hour window no worker starts on it, and its running workers stop with their state saved and resume when the window allows, like a shed. This is the stricter reading of D10.4 and protects the owner.

### Behaviour

- **A19. No launch without the orchestrator.** When no pass is possible, `idlemax plan` lists the ready tasks in priority order, labelled "not judged by the orchestrator", and no slot is filled from that list.
- **A20. When the pass runs.** Only when its inputs changed (tasks, usable accounts, models, `rules.md`, an opened slot), at most once an hour by itself, and on `idlemax plan --refresh`. A task the pass sent to "needs you" stays there until the task changes.
- **A21. Models.** The orchestrator uses the Claude alias `opus`. Claude workers use `sonnet` for the standard tier and `haiku` for the small tier. Other providers' model keys ship unset until a measured run proves a name. Workers start with Claude and Codex; another CLI joins when its lean launch and resume are proven.
- **A22. Notifications.** The inbox entry is always written. The desktop notification follows `agent.notify`. A finished plain job joins the "task finished" batch instead of its own notice. The daily summary goes out at 09:00 in the coordinator's local time, and Spec 5 sets the server's time zone to the owner's.
- **A23. Devices.** When Spec 5 lands every device is off, the Mac included, and an off device with switched-on projects raises a "needs you" entry. `device off` stops that device's workers with their state saved, and `device on` resumes them. A `production` device always runs workers headless. A device's limits change on that device; the dashboard shows other devices' limits read-only.
- **A24. Smaller readings.** The task type is the Task's `source`. Branches are never stacked for now. A `rejected` run does not count toward the cap of 3. "Lowest-priority task" means the priority of the task's Project. A `pause` freezes a worker in place.
- **A25. Starting numbers.** Every number a spec proposed as a starting value is accepted as a setting or named constant, to be tuned from measured runs: for example swap-in of 10 and 100 pages a second, 512 MiB per worker, 30 minutes as "recently", a 10 percent safety margin, and the 24-hour day counted back from the weekly reset.

## Where this meets the existing code

| Area | Files |
|---|---|
| Guard decision, pauses on memory pressure today | `crates/idlemax-core/src/guard.rs` |
| Slots, RAM and account headroom per device | `crates/idlemax-core/src/capacity/` |
| Quota windows, reserves, timing and the interactive hold | `crates/idlemax-core/src/quota.rs`, `crates/idlemax-core/src/quota/rules.rs` |
| Advisor rules, the AI pass and its prompt | `crates/idlemax-core/src/advice/` |
| Advisor adapters: Claude pass, GitHub, sessions, job templates | `crates/idlemax/src/advisor/` |
| Job spec (no model or project field today) | `crates/idlemax-core/src/types/job.rs` |
| Pause, resume and stop-all semantics | `crates/idlemax-core/src/control/` |
| Job runner, quota probe, notifications, the agent loop | `crates/idlemax/src/agent/` |
| Store migrations, HTTP API, dashboard | `crates/idlemax/src/coord/store/schema.rs`, `crates/idlemax/src/coord/api.rs`, `crates/idlemax/src/coord/web.rs` |
| CLI and TUI | `crates/idlemax/src/cli.rs`, `crates/idlemax/src/cli/`, `crates/idlemax/src/tui/` |
| Wire formats and control semantics | `docs/API.md` |

Related roadmap items: 4, 7 and 31 (run the current build for real on the Mac) come before Spec 1 can be proven live.
Item 30 (tune the laptop guard) is answered by D14.
Item 11 (learn token costs) is answered by D10.
Items 1 and 2 (the Linux agent on a server, the coordinator on the private network) come before Spec 5.
