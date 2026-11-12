# 13. Glossary 📒

**What you will learn:** what every idlemax word means, in one line each, with a link to where it is explained.

| Word | Meaning |
|---|---|
| **Account** | one AI login, named in `[[quota.accounts]]`; one name means one login on every device ([6](06-quota-and-budget.md)) |
| **Account day** | a 24-hour slot counted back from an account's weekly reset |
| **Advisor** | turns cluster state, PRs and sessions into recommendations you accept or dismiss ([7](07-dashboard-tui-mcp.md#the-advisor)) |
| **Agent** | `idlemax agent`, one per device: samples the machine, runs the guard and the jobs ([2](02-first-run.md)) |
| **Allowlist** | `agent.command_allowlist`: the exact programs a job may run on a device; empty allows nothing |
| **Base branch** | the branch a project's tasks start from |
| **Base check** | the project's focused check run on the base commit, to find what is broken ([5](05-dispatcher.md#the-base-check)) |
| **Brief** | the written instruction a worker starts with: goal, files in scope, what not to touch, the check, "focused tests only" |
| **Budget** | the quota idlemax may spend on one account today ([6](06-quota-and-budget.md#the-budget-explained-with-numbers)) |
| **Coordinator** | `idlemax coord`: the queue, database, API and dashboard; one for all devices |
| **Core paths** | a project's shared code; a diff there gets an independent review |
| **Cost class** | a job's kind of load: `cpu_light`, `cpu_heavy`, `io` or `ai` |
| **Credential (device)** | a device's own key that opens only the agent routes ([8](08-servers.md)) |
| **Device** | a machine that runs an idlemax agent |
| **Dispatcher** | the part of idlemax that puts AI workers on leftover tasks ([5](05-dispatcher.md)) |
| **Emergency** | the third worker line: every worker stops at once, state saved |
| **Guard** | the agent's per-sample verdict for plain jobs: run, hold, pause or kill ([3](03-concepts.md)) |
| **Headless** | a worker run as a child of the agent with no Herdr pane |
| **Heartbeat** | the agent's regular report to the coordinator (every 30 s by default) |
| **Hold** | start nothing new; running work continues (a guard verdict and a worker line) |
| **Inbox** | the folder of result notes and notices on each device |
| **Job** | a command idlemax runs on spare capacity ([4](04-jobs.md)) |
| **Key** | a job's idempotency key; adding it again while the job is unfinished returns that job |
| **Kill switch** | the file `$IDLEMAX_HOME/PAUSE`, or `stop-all` |
| **Lease** | a device's claim on a job; it lapses after 120 s without a heartbeat |
| **Lines** | hold, shed and emergency: the three thresholds workers follow |
| **Measuring week** | an account's first week of Runs, capped at `quota.measuring_week_cap_pct` (10%) of each weekly window |
| **Mode** | the dispatcher's setting: `off`, `propose` or `auto` |
| **Needs you** | a list of tasks idlemax could not do alone, each with the reason |
| **Notice** | one of the three things that interrupt you: finished tasks, something needs you, the daily summary |
| **Off-limits** | paths of a project a worker must not touch: yours, plus `.github/`, `.gitlab-ci.yml`, `*.env` and `secrets/` unless `default-off-limits` is `off` |
| **Orchestrator** | the short Claude Opus pass that picks task, account and model for each open slot |
| **Owner** | you: the person whose machines and quota these are |
| **Owner session** | an agent session or app you started; idlemax never stops one, edits its files or shares its folder |
| **Parked** | a job interrupted `max_attempts` times for reasons not its own; needs you |
| **Plan** | what the dispatcher would launch now, per device, and why (`idlemax plan`) |
| **Profile** | a guard and lines preset: `laptop`, `workbox` or `production` |
| **Project** | a git repository on a device that idlemax knows; off until you switch it on |
| **PSI** | Linux pressure stall information: how long tasks waited for memory or I/O |
| **Reserve** | the part of a quota window kept for you |
| **Review cap** | `workers.review_cap` (3): per project, the tasks waiting for your review plus the Runs in flight may number at most this |
| **Room** | a device's worker line state, as `idlemax device list` shows it |
| **rules.md** | the orchestrator's judgment rules, yours to edit |
| **Run** | the record of one worker on one task, from start to verdict |
| **Schedule** | a job repeated on an interval ([4](04-jobs.md#repeat-a-job-schedules)) |
| **Shed** | the second worker line: idlemax stops its newest worker, one at a time |
| **Slot** | room for one more job (guard) or one more worker (lines) |
| **Surplus** | the quota that would expire unused at the reset, given your usual use |
| **Task** | one small piece of work in a project with a check that proves it done |
| **Tier** | a worker's model class: `small` (Haiku class) or `standard` (Sonnet class) |
| **TOON** | the compact text format idlemax prints when piped |
| **Verdict** | how idlemax judged a finished task: `clean`, `needs_you` or `rejected` |
| **Window** | a span over which a provider counts quota: 5-hour rolling or weekly |
| **Worker** | one coding agent process idlemax started for one task |
| **Worktree** | the separate git working folder each worker gets, under `$IDLEMAX_HOME/worktrees` |

Next: [Safety and privacy](14-safety-and-privacy.md)
