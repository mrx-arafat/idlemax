//! Domain: detection, until a sandbox lands (A13), of a worker writing idlemax's own files
//! (red team cases 2 and 3). The agent takes the digest of each guarded file in the state
//! dir when a worker starts and again when its turn is checked; a change whose new digest
//! the owner-writes ledger does not hold was not made by an owner command. The worker runs
//! as the same user and could write the ledger too, so this detects, it does not prevent.
//! Pure.

/// The files in the state dir a worker must never change.
pub const GUARDED: [&str; 3] = ["config.toml", "rules.md", "token"];
/// The ledger in the state dir where every owner write of a guarded file adds its digest.
pub const LEDGER: &str = ".owner-writes";
/// The digest of a guarded file that does not exist.
pub const ABSENT: &str = "absent";

/// The digest of a guarded file's content.
pub fn digest(content: &[u8]) -> String {
    crate::dispatch::fnv64(&[&String::from_utf8_lossy(content)])
}

/// The ledger line of one owner write.
pub fn ledger_line(name: &str, digest: &str) -> String {
    format!("{name} {digest}\n")
}

/// The guarded files whose digest changed between `before` and `after` to one the `ledger`
/// does not hold for that file. Each list holds (file name, digest).
pub fn tampered(
    before: &[(String, String)],
    after: &[(String, String)],
    ledger: &str,
) -> Vec<String> {
    let owned = |name: &str, digest: &str| {
        ledger
            .lines()
            .filter_map(|l| l.split_once(' '))
            .any(|(n, d)| n == name && d == digest)
    };
    after
        .iter()
        .filter(|(name, now)| {
            before
                .iter()
                .find(|(n, _)| n == name)
                .is_some_and(|(_, was)| was != now && !owned(name, now))
        })
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn only_a_change_the_owner_ledger_does_not_hold_is_tampering() {
        let before = pairs(&[("config.toml", "a"), ("rules.md", "b"), ("token", "c")]);
        let same = before.clone();
        assert!(tampered(&before, &same, "").is_empty());
        let after = pairs(&[("config.toml", "a2"), ("rules.md", "b2"), ("token", "c")]);
        // `idlemax mode auto` wrote config.toml as a2; nobody recorded rules.md as b2.
        let ledger = format!(
            "{}{}",
            ledger_line("config.toml", "a2"),
            ledger_line("rules.md", "zz")
        );
        assert_eq!(tampered(&before, &after, &ledger), ["rules.md"]);
        // No snapshot at the start (an agent restarted mid-run): nothing to compare.
        assert!(tampered(&[], &after, "").is_empty());
        assert_ne!(digest(b"x"), digest(b"y"));
    }
}
