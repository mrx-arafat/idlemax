use super::*;
use crate::types::QuotaWindowKind;

const NOW: UnixMs = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

fn weekly(used: f64, at: UnixMs, reset: UnixMs) -> QuotaSnapshot {
    QuotaSnapshot {
        account: "max".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: used,
        resets_at_ms: Some(reset),
        source: Default::default(),
        at_ms: at,
    }
}

#[test]
fn a_day_slot_counts_back_from_the_reset() {
    let slot = day_slot(NOW + 49 * HOUR, NOW);
    assert_eq!(slot.days_left, 3);
    assert_eq!(slot.end_ms, NOW + HOUR);
    assert_eq!(slot.start_ms, NOW + HOUR - DAY_MS);
}

#[test]
fn a_day_splits_its_rise_between_idlemax_and_the_owner() {
    let reset = NOW + 3 * DAY_MS;
    let slot = DaySlot {
        start_ms: NOW,
        end_ms: NOW + DAY_MS,
        days_left: 3,
    };
    let readings = [
        weekly(20.0, NOW, reset),
        weekly(31.0, NOW + 5 * HOUR, reset),
    ];
    let day = account_day("max", &slot, &readings, &[(NOW + 2 * HOUR, 4.0)]);
    assert_eq!(
        (day.total_pct, day.idlemax_pct, day.owner_pct, day.runs),
        (Some(11.0), Some(4.0), Some(7.0), Some(1))
    );
    assert!(day.closed && day.spendable_pct.is_none());

    let empty = account_day("max", &slot, &readings[..1], &[]);
    assert_eq!(empty.owner_pct, None);
    let used = owner_use(&[empty]);
    assert_eq!((used.days_known, used.usual_daily_pct), (0, None));
}

#[test]
fn the_usual_use_is_the_mean_of_the_newest_seven_known_days() {
    let days: Vec<AccountDay> = [7.0, 5.0, 0.0, 9.0, 6.0, 8.0, 7.0]
        .iter()
        .enumerate()
        .map(|(i, pct)| AccountDay {
            account: "max".into(),
            day_start_ms: NOW - (i as i64 + 1) * DAY_MS,
            owner_pct: Some(*pct),
            closed: true,
            ..AccountDay::default()
        })
        .collect();
    let used = owner_use(&days);
    assert_eq!((used.days_known, used.usual_daily_pct), (7, Some(6.0)));
}
