use super::*;
use crate::quota::rolling_5h;

const MIN: UnixMs = 60_000;
const T0: UnixMs = 1_800_000_000_000;
const RESET: UnixMs = T0 + 3 * 86_400_000;
const WEEKLY: QuotaWindowKind = QuotaWindowKind::Weekly;

fn snap(window: QuotaWindowKind, used: f64, at: UnixMs) -> QuotaSnapshot {
    QuotaSnapshot {
        account: "max".into(),
        window,
        used_pct: used,
        resets_at_ms: Some(RESET),
        source: Default::default(),
        at_ms: at,
    }
}

fn run(task_type: &str, from: i64, to: i64, before: f64, after: f64) -> RunSample {
    RunSample {
        provider: Provider::Claude,
        account: "max".into(),
        model: "sonnet".into(),
        task_type: task_type.into(),
        started_ms: T0 + from * MIN,
        ended_ms: Some(T0 + to * MIN),
        launches: 1,
        before: vec![snap(WEEKLY, before, T0 + from * MIN)],
        after: vec![snap(WEEKLY, after, T0 + to * MIN)],
    }
}

fn weekly(costs: &[RunCost]) -> Vec<Option<f64>> {
    costs
        .iter()
        .filter(|c| c.window == WEEKLY)
        .map(|c| c.pct)
        .collect()
}

fn key(task_type: &str) -> CostKey {
    CostKey {
        provider: Provider::Claude,
        account: "max".into(),
        model: "sonnet".into(),
        task_type: task_type.into(),
    }
}

#[test]
fn a_lone_run_costs_its_rise_on_each_window() {
    let mut r = run("roadmap", 0, 10, 40.0, 42.5);
    r.before.push(snap(rolling_5h(), 10.0, T0));
    r.after.push(snap(rolling_5h(), 16.0, T0 + 10 * MIN));
    let costs = run_costs(&[r]);
    let five: Vec<Option<f64>> = costs
        .iter()
        .filter(|c| c.window == rolling_5h())
        .map(|c| c.pct)
        .collect();
    assert_eq!(weekly(&costs), [Some(2.5)]);
    assert_eq!(five, [Some(6.0)]);
}

#[test]
fn overlapping_runs_split_the_shared_rise() {
    let a = run("roadmap", 0, 10, 40.0, 43.0);
    let b = run("roadmap", 5, 15, 41.0, 44.0);
    assert_eq!(weekly(&run_costs(&[a, b])), [Some(2.0), Some(2.0)]);
}

#[test]
fn unmeasurable_runs_have_no_cost_and_their_key_keeps_the_seed() {
    let cfg = QuotaConfig::default();
    let mut stale = run("roadmap", 0, 10, 40.0, 42.0);
    stale.after[0].at_ms = T0 + 9 * MIN;
    let mut twice = run("session", 0, 10, 40.0, 42.0);
    twice.launches = 2;
    let samples = [stale, twice];
    let costs = run_costs(&samples);
    assert_eq!(weekly(&costs), [None, None]);
    let table = cost_table(&samples, &costs);
    for k in ["roadmap", "session"] {
        let e = expected(&table, &key(k), Tier::Standard, &cfg);
        assert_eq!((e.pct, e.basis), (8.0, CostBasis::Seed));
    }
}

#[test]
fn five_samples_replace_the_seed_and_a_model_pools_its_task_types() {
    let cfg = QuotaConfig::default();
    let rises = [1.0, 2.0, 1.0, 2.0, 1.5];
    let samples: Vec<RunSample> = rises
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let from = i as i64 * 20;
            run("roadmap", from, from + 10, 40.0, 40.0 + r)
        })
        .collect();
    let table = cost_table(&samples, &run_costs(&samples));
    let e = expected(&table, &key("roadmap"), Tier::Standard, &cfg);
    assert_eq!((e.pct, e.basis, e.samples), (1.5, CostBasis::Measured, 5));

    let four = &samples[..4];
    let table = cost_table(four, &run_costs(four));
    let e = expected(&table, &key("roadmap"), Tier::Standard, &cfg);
    assert_eq!((e.pct, e.basis), (8.0, CostBasis::Seed));

    let mut mixed = samples.clone();
    mixed[3].task_type = "session".into();
    mixed[4].task_type = "session".into();
    let table = cost_table(&mixed, &run_costs(&mixed));
    let e = expected(&table, &key("github_issue"), Tier::Standard, &cfg);
    assert_eq!((e.pct, e.basis), (1.5, CostBasis::SameModel));
}

#[test]
fn a_claude_account_shows_one_line_per_tier() {
    let cfg = QuotaConfig::default();
    let lines = tier_costs(Provider::Claude, "max", &CostTable::default(), &cfg);
    let shown: Vec<(Tier, &str, f64, CostBasis, u32)> = lines
        .iter()
        .map(|l| (l.tier, l.model.as_str(), l.est_pct, l.basis, l.samples))
        .collect();
    assert_eq!(
        shown,
        [
            (Tier::Small, "haiku", 3.0, CostBasis::Seed, 0),
            (Tier::Standard, "sonnet", 8.0, CostBasis::Seed, 0),
        ]
    );
}
