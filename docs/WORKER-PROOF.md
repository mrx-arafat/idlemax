# Worker proof

The written manual check of each worker CLI, each fence and each reviewer (dispatcher Spec 2, D22).
A CLI enters `workers.clis` and `agent.command_allowlist`, and a reviewer enters `workers.reviewers`, only when every row of its table passed.
Every fence here is a soft fence until the owner answers A13: the worker runs as the owner's own user.

Owner's OK for the soft fences: (date, filled in by Task 12)

## Setup

Run everything in a throwaway setup, never against `~/.idlemax`.

```sh
REAL_HOME=$HOME
T=$(mktemp -d) && export IDLEMAX_HOME="${T:?}/state" && mkdir -p "$IDLEMAX_HOME"
test -n "$IDLEMAX_HOME" && test "$IDLEMAX_HOME" != "$REAL_HOME/.idlemax" && test -d "$IDLEMAX_HOME" || exit 1
target/debug/idlemax init
target/debug/idlemax config set coordinator.listen 127.0.0.1:17878
target/debug/idlemax config set coordinator.url http://127.0.0.1:17878
target/debug/idlemax config set agent.listen 127.0.0.1:17879
git init "$T/repo" && echo one > "$T/repo/file.txt" && git -C "$T/repo" add file.txt && git -C "$T/repo" commit -m one
git init --bare "$T/remote.git" && git -C "$T/repo" remote add origin "$T/remote.git"
```

`HOME` stays the real one, because the agent CLI needs its login.
No `launchctl`, no service install and no statusline or hook install is run.
The sample task adds one line to `file.txt` and commits it.

## Before any CLI: the pane's environment

| Step | Command | Expected | Result |
|---|---|---|---|
| Names a plain new pane would report | `herdr pane split --pane <a pane of a scratch tab> --direction down --no-focus`, then in it `env \| cut -d= -f1` and compare with the names `env_problems` reports (`IDLEMAX_TOKEN`, `SSH_AUTH_SOCK`, `AWS_*`, `*_TOKEN`, `*_KEY`, `*_SECRET`, `*_PASSWORD`, `*_CREDENTIALS`, `*DATABASE_URL*`) | A list of names only, never values. A name the Herdr server passes on is blanked by the launch; a name a shell start-up file exports is guarded there with `[ -z "$IDLEMAX_JOB_ID" ]` before any CLI is tried | |
| JSON shapes | `herdr workspace list`, `herdr tab list --workspace <id>`, `herdr pane list`, `herdr agent list`, `herdr pane process-info --pane <id>`, `herdr agent read <id> --source recent --lines 40` | Each prints `{"id": ..., "result": {...}}` with the fields `crates/idlemax/src/agent/worker/herdr/parse.rs` reads; `agent read` gives `result.text` or `result.lines` | |
| Env pairs reach the pane | `herdr pane split ... --env IDLEMAX_JOB_ID=1 --env GH_TOKEN= --no-focus`, then `env \| grep -E '^(IDLEMAX_JOB_ID\|GH_TOKEN)='` in it | `IDLEMAX_JOB_ID=1` and an empty `GH_TOKEN` | |

## Claude

| Step | Command | Expected | Result |
|---|---|---|---|
| A start takes no focus | Launch one worker on the sample task | The owner's focused pane and tab stay focused; the worker appears in the `idlemax workers` workspace, tab `im: <Project>` | |
| First start in a new folder | The worker starts in a worktree Claude has never seen | It reaches `idle` with no trust or permission dialog | |
| Session id | `herdr agent list` | `agent_session.value` equals the `--session-id` idlemax passed and the Run's `session` | |
| Environment check | The launch's own check | `env_problems` finds nothing in the agent's real environment, so the brief is sent | |
| A working commit | Let the turn finish | One commit on `idlemax/<task>`; note whether commit signing asked for anything | |
| Push is refused | In the worker's worktree, `git push origin HEAD` | Fails on `idlemax-no-push://origin`; `git -C $T/remote.git for-each-ref` is empty | |
| No MCP helper process | `ps` of the worker's process tree | No process of the owner's personal MCP servers | |
| Background class | `ps -o pid,pri,command -p <shell pid>`, then time the sample task | The shell runs in the background class; write down the task's time | |
| Pause and the PAUSE file | `idlemax pause`, then `idlemax resume`; `touch $IDLEMAX_HOME/PAUSE`, then remove it | The same turn freezes and thaws each time | |
| Stop | `idlemax stop` | The pane closes within seconds, no worker process is left, uncommitted edits stay in the worktree | |
| Continued session | `idlemax start` | The worker reopens with `--resume <session>` and continues the same turn | |
| Memory | Sum of RSS of the worker's process set at its peak | A figure for `dispatcher.worker_mem_mib` | |
| Tokens | The first turn's input tokens, and a turn that ends with the owner's hooks on | Figures for Open Question 4 | |
| Installed service | One run started by the installed service | Same results as above | |

## Codex

| Step | Command | Expected | Result |
|---|---|---|---|
| MCP server names | `codex mcp list` | The first column lists the configured servers; each gets `-c mcp_servers.<name>.enabled=false` | |
| A start takes no focus | Launch one worker on the sample task | As for Claude | |
| First start in a new folder | As for Claude | Reaches `idle` with no dialog | |
| Session id | `herdr agent list` | `agent_session.value` is set and stored on the Run | |
| Environment check | The launch's own check | Nothing reported | |
| A working commit | Let the turn finish | One commit; the `workspace-write` sandbox with `--add-dir <git common dir>` lets it write objects and refs | |
| Push is refused | `git push origin HEAD` in the worktree | Fails; the bare remote has no new ref | |
| Network | A network call from inside the sandbox | Refused by Codex's sandbox | |
| No MCP helper process | `ps` of the worker's process tree | None of the owner's servers runs | |
| Background class | As for Claude | Shell in the background class; the task's time written down | |
| Pause and the PAUSE file | As for Claude | Freezes and thaws the same turn | |
| Stop | `idlemax stop` | As for Claude | |
| Continued session | `idlemax start` | The worker reopens with `resume <session>` and continues | |
| Memory | As for Claude | A figure | |
| Tokens | As for Claude | Figures | |
| Installed service | As for Claude | Same results | |

## Headless Claude

The headless host (dispatcher Spec 5 Task 8) runs the worker with no Herdr: `claude -p` as a child of the agent, its output in the job log.
Run these on a server in the throwaway setup above, with `workers.host headless`; Task 12 of Spec 5 fills the results.

| Step | Command | Expected | Result |
|---|---|---|---|
| A start | Launch one worker on the sample task | The worker job runs with no pane; `herdr` is never called | |
| The log | `idlemax logs <job> --follow` on that device | The worker's output appears and the command returns at the footer when the worker exits | |
| A working commit | Let the turn finish | One commit on `idlemax/<task>` | |
| Push is refused | In the worker's worktree, `git push origin HEAD` | Fails on `idlemax-no-push://origin`; the bare remote has no new ref | |
| No MCP helper process | `ps` of the worker's process tree | No process of the owner's personal MCP servers | |
| Stop | `idlemax stop` | Every process of the worker's group is gone within 10 s, uncommitted edits stay in the worktree | |
| Continued session | `idlemax start` | The worker starts again with `--resume <session>` and the same session id | |
| An agent restart | Restart the agent in the middle of a turn | The worker is not taken back; its job ends `killed` and retryable, no process with its tag is left, and the Run continues with `--resume` | |

## Reviewers

| Reviewer | Command | Expected | Result |
|---|---|---|---|
| Claude | One real review of a sample diff with the reviewer arguments of Task 8 | The JSON answer `{"verdict": "approve" or "concern", "concern": "..."}`, and its cost | |
| Codex | As above with `codex exec --sandbox read-only --ephemeral ...` | The same JSON answer, and its cost | |

## Not fenced

Credentials on disk or in the keychain (ssh keys, the `gh` login, cloud CLI logins), the idlemax token file and `coord.db` stay readable to a worker that goes looking, and the git push block can be undone on purpose (Open Question 1 of Spec 2).
