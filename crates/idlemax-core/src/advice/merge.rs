//! Domain: folds the newest advisor inputs of every device into the one GitHub snapshot
//! and session list the rules engine reads. Pure: time is a parameter.

use std::collections::BTreeMap;

use super::inputs::{GithubSnapshot, GithubStatus, SessionMeta, StoredInputs};
use crate::types::UnixMs;

/// Copies older than this are ignored: the device stopped reporting, and the rules
/// already drop sessions quieter than the same 3 days.
pub const INPUTS_MAX_AGE_MS: i64 = 3 * 24 * 3_600_000;

/// One snapshot and one session list from the per-device copies.
///
/// GitHub data belongs to the `gh` login, not to a device: the newest copy that read it
/// fine wins, so a server without `gh` never masks the Mac. With none fine, the newest
/// failure shows (the rules warn about it), else GitHub counts as missing.
/// Sessions of all devices are listed, newest first, once per tool and id.
pub fn merge_inputs(copies: &[StoredInputs], now_ms: UnixMs) -> (GithubSnapshot, Vec<SessionMeta>) {
    let mut fresh: Vec<&StoredInputs> = copies
        .iter()
        .filter(|c| now_ms - c.received_ms <= INPUTS_MAX_AGE_MS)
        .collect();
    fresh.sort_by_key(|c| std::cmp::Reverse(c.received_ms));
    let github = fresh
        .iter()
        .map(|c| &c.inputs.github)
        .find(|g| g.status == GithubStatus::Ok)
        .or_else(|| {
            fresh
                .iter()
                .map(|c| &c.inputs.github)
                .find(|g| matches!(g.status, GithubStatus::Failed(_)))
        })
        .cloned()
        .unwrap_or_default();
    let mut sessions: BTreeMap<(&str, &str), &SessionMeta> = BTreeMap::new();
    for s in fresh.iter().flat_map(|c| &c.inputs.sessions) {
        let slot = sessions.entry((&s.tool, &s.id)).or_insert(s);
        if s.last_activity_ms > slot.last_activity_ms {
            *slot = s;
        }
    }
    let mut sessions: Vec<SessionMeta> = sessions.into_values().cloned().collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
    (github, sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advice::inputs::{AdvisorInputs, GhItem};

    const NOW: UnixMs = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn copy(
        device: &str,
        age_ms: i64,
        github: GithubSnapshot,
        ids: &[(&str, i64)],
    ) -> StoredInputs {
        StoredInputs {
            received_ms: NOW - age_ms,
            inputs: AdvisorInputs {
                device: device.into(),
                github,
                sessions: ids
                    .iter()
                    .map(|(id, at)| SessionMeta {
                        tool: "claude".into(),
                        id: (*id).into(),
                        last_activity_ms: *at,
                        ..SessionMeta::default()
                    })
                    .collect(),
            },
        }
    }

    fn snapshot(status: GithubStatus, pr: u64) -> GithubSnapshot {
        GithubSnapshot {
            status,
            assigned_prs: vec![GhItem {
                number: pr,
                ..GhItem::default()
            }],
            ..GithubSnapshot::default()
        }
    }

    #[test]
    fn github_comes_from_the_newest_good_copy_and_stale_copies_are_ignored() {
        let ok_old = copy("mac", 5 * HOUR, snapshot(GithubStatus::Ok, 1), &[]);
        let ok_new = copy("mac2", 2 * HOUR, snapshot(GithubStatus::Ok, 2), &[]);
        let server = copy("srv", HOUR, GithubSnapshot::default(), &[]);
        let ancient = copy("old", 4 * 24 * HOUR, snapshot(GithubStatus::Ok, 9), &[]);

        let (gh, _) = merge_inputs(&[ok_old, ok_new, server.clone(), ancient.clone()], NOW);
        assert_eq!(
            (gh.status, gh.assigned_prs[0].number),
            (GithubStatus::Ok, 2)
        );

        let failed = copy(
            "mac",
            3 * HOUR,
            snapshot(GithubStatus::Failed("auth".into()), 3),
            &[],
        );
        let (gh, _) = merge_inputs(&[server.clone(), failed], NOW);
        assert_eq!(gh.status, GithubStatus::Failed("auth".into()));

        let (gh, _) = merge_inputs(&[server, ancient], NOW);
        assert_eq!(gh, GithubSnapshot::default());
    }

    #[test]
    fn sessions_are_listed_once_newest_first_across_devices() {
        let a = copy(
            "mac",
            2 * HOUR,
            GithubSnapshot::default(),
            &[("s1", 10), ("s2", 30)],
        );
        let b = copy("srv", HOUR, GithubSnapshot::default(), &[("s1", 20)]);
        let (_, sessions) = merge_inputs(&[a, b], NOW);
        let got: Vec<(&str, i64)> = sessions
            .iter()
            .map(|s| (s.id.as_str(), s.last_activity_ms))
            .collect();
        assert_eq!(got, vec![("s2", 30), ("s1", 20)]);
    }
}
