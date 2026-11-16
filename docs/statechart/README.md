# Job lifecycle statechart

The chart under [A job's life](../../README.md#a-jobs-life) in the top-level README is drawn from a TLA+ model and checked with the TLC model checker.
Change the model here first, then draw the chart again, so the picture never drifts from the rules.

## Files

- `JobLifecycle.tla`: one plain job on one device, as [the jobs chapter](../manual/04-jobs.md#what-a-jobs-status-means) and `crates/idlemax-core/src/scheduler/update.rs` describe it.
- `JobLifecycle.cfg`: the rules TLC checks, with `MaxAttempts = 2`.
- `readme-view.mjs`: the words and colors of the README chart.

## The rules TLC checks

- `DeviceHeldExactlyWhileActive`: a job holds a device exactly while it is leased, running or paused.
- `RunsOnlyWhenCalm`: a job never runs while the machine is busy.
- `ParkedOnlyAfterLastAttempt`: a job is parked only after its last attempt.
- `TypeOK` and no deadlock: every state has a way on, and the final states stay put.

All of them hold in all 34 states of the model.

## What the chart leaves out

The README chart hides two kinds of steps, and the README says them in words:

- Cancel: you can cancel at any point, and the job ends as cancelled.
- A device dropping the job while it is starting or paused: it goes the same way as from Running.

The model itself does not cover several devices, a late result from a run whose lease expired, the retry backoff, AI jobs (one attempt), worker jobs, or queueing a parked job again.

## Draw it again

Install the `statechart` skill from [cursor/plugins](https://github.com/cursor/plugins/tree/main/pstack/skills/statechart), then run it in this folder with a new `--out` folder:

```sh
<skill dir>/scripts/statechart JobLifecycle.tla --view readme-view.mjs --out /tmp/job-chart-1
```

The first run installs TLC and Java with mise.
To skip mise, set `TLA2TOOLS` to a `tla2tools.jar` (v1.7.4) and `JAVA` to a Java 11 or later binary.

Then copy `chart.mmd` into the README chart:

1. Drop its `Legend` block and its `class lgN` lines; the README says what the colors mean in one line.
2. Keep the five `classDef` lines at the end of the README chart: they give GitHub the colors, with dark text so the chart reads in dark mode too.
