//! Tests: the scope check, the risk rule and the verdict of a finished task.

use super::*;
use crate::worker::report::{FileDiff, ReviewFacts};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn spec() -> WorkerSpec {
    WorkerSpec {
        scope_paths: strings(&["src/sync.rs", "tests/sync/"]),
        off_limits: strings(&["migrations/"]),
        core_paths: strings(&["crates/core"]),
        check: strings(&["make", "check"]),
        review_diff_lines: 300,
        ..Default::default()
    }
}

fn diff(paths: &[&str], lines: u64) -> DiffStat {
    DiffStat {
        files: paths
            .iter()
            .map(|p| FileDiff {
                path: p.to_string(),
                added: lines,
                removed: 0,
            })
            .collect(),
    }
}

/// One small in-scope commit, checked before and after.
fn good() -> CheckFacts {
    CheckFacts {
        commits: 1,
        diff: diff(&["src/sync.rs"], 5),
        summary: Some("fix the flaky sync test".into()),
        baseline_before: Some(0),
        baseline_after: Some(0),
        ..Default::default()
    }
}

#[test]
fn scope_findings_flags_off_limits_and_outside_paths() {
    let s = spec();
    let found = scope_findings(
        &strings(&["migrations/001.sql", "README.md", "tests/sync/a.rs"]),
        &s.scope_paths,
        &s.off_limits,
    );
    assert_eq!(found.off_limits, ["migrations/001.sql"]);
    assert_eq!(found.outside, ["migrations/001.sql", "README.md"]);
    let inside = scope_findings(
        &strings(&["src/sync.rs", "tests/sync/b.rs"]),
        &s.scope_paths,
        &s.off_limits,
    );
    assert_eq!(inside, ScopeFindings::default());
    assert!(scope_findings(&[], &[], &[]).no_scope);
}

#[test]
fn scope_paths_follow_gitignore_rules() {
    // The live Run: the orchestrator named ["*.py", "tests"].
    let scope = strings(&["*.py", "tests"]);
    let changed = strings(&[
        "calc/__init__.py",
        "tests/test_calc.py",
        "calc/__pycache__/__init__.cpython-313.pyc",
    ]);
    let found = scope_findings(&changed, &scope, &[]);
    assert_eq!(found.outside, ["calc/__pycache__/__init__.cpython-313.pyc"]);
    // A pattern with a slash is anchored at the root; a plain name matches at any depth.
    let anchored = strings(&["src/*.rs", "/docs"]);
    let found = scope_findings(
        &strings(&[
            "src/a.rs",
            "lib/src/a.rs",
            "src/x/a.rs",
            "docs/a.md",
            "x/docs/a.md",
        ]),
        &anchored,
        &strings(&["secret"]),
    );
    assert_eq!(found.outside, ["lib/src/a.rs", "src/x/a.rs", "x/docs/a.md"]);
    assert!(found.off_limits.is_empty());
    let deep = scope_findings(&strings(&["a/secret/k"]), &[], &strings(&["secret/"]));
    assert_eq!(deep.off_limits, ["a/secret/k"]);
}

#[test]
fn a_scope_name_means_the_top_level_and_a_scope_glob_a_file_name() {
    let scope = strings(&["tests", "*.py"]);
    let changed = strings(&[
        "tests/a.rs",
        "vendor/lib/tests/x.rs",
        "calc/__init__.py",
        "x.py/evil.rs",
    ]);
    let found = scope_findings(&changed, &scope, &strings(&["tests"]));
    assert_eq!(found.outside, ["vendor/lib/tests/x.rs", "x.py/evil.rs"]);
    // Off-limits paths keep the wide match: a name at any depth.
    assert_eq!(found.off_limits, ["tests/a.rs", "vendor/lib/tests/x.rs"]);
}

#[test]
fn a_glob_with_many_stars_is_matched_in_linear_time() {
    let name = "a".repeat(200);
    let pattern = format!("{}b", "a*".repeat(40));
    let started = std::time::Instant::now();
    assert!(!super::glob(pattern.as_bytes(), name.as_bytes()));
    assert!(super::glob(b"a*a*a", name.as_bytes()));
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
}

#[test]
fn risk_is_high_for_a_big_diff_core_code_or_no_check() {
    let small = diff(&["src/sync.rs"], 5);
    let core = strings(&["crates/core"]);
    assert!(risk_is_high(
        &diff(&["src/sync.rs"], 301),
        300,
        &core,
        true,
        Some(0)
    ));
    assert!(!risk_is_high(
        &diff(&["src/sync.rs"], 300),
        300,
        &core,
        true,
        Some(0)
    ));
    assert!(risk_is_high(
        &diff(&["crates/core/lib.rs"], 1),
        300,
        &core,
        true,
        Some(0)
    ));
    assert!(risk_is_high(&small, 300, &core, false, Some(0)));
    assert!(risk_is_high(&small, 300, &core, true, None));
    assert!(!risk_is_high(&small, 300, &core, true, Some(0)));
}

fn judged(facts: CheckFacts) -> Judged {
    verdict(&facts, &spec())
}

fn concern(j: &Judged) -> &str {
    j.concern.as_deref().unwrap_or_default()
}

#[test]
fn a_turn_that_changed_idlemax_own_files_is_rejected() {
    // Red team cases 2 and 3: the worker wrote config.toml without an owner command.
    let j = judged(CheckFacts {
        tampered: vec!["config.toml".into()],
        ..good()
    });
    assert_eq!(j.verdict, Verdict::Rejected);
    assert!(
        concern(&j).contains("changed idlemax's own files while it ran: config.toml"),
        "{}",
        concern(&j)
    );
}

#[test]
fn rejected_for_no_commit_an_off_limits_path_or_a_check_it_broke() {
    let none = judged(CheckFacts {
        commits: 0,
        diff: DiffStat::default(),
        ..good()
    });
    assert_eq!(none.verdict, Verdict::Rejected);
    assert!(concern(&none).contains("no commit"));

    let off = judged(CheckFacts {
        diff: diff(&["src/sync.rs", "migrations/001.sql"], 1),
        ..good()
    });
    assert_eq!(off.verdict, Verdict::Rejected);
    assert!(concern(&off).contains("migrations/001.sql"));
    // Rejected wins over the needs-you finding of the same path.
    assert!(!concern(&off).contains("outside the scope"));

    let broke = judged(CheckFacts {
        baseline_after: Some(2),
        ..good()
    });
    assert_eq!(broke.verdict, Verdict::Rejected);
    assert!(concern(&broke).contains("passed before"));
}

#[test]
fn needs_you_names_each_concern() {
    let cases = [
        (
            CheckFacts {
                diff: diff(&["README.md"], 1),
                ..good()
            },
            "outside the scope: README.md",
        ),
        (
            CheckFacts {
                modified: strings(&["src/sync.rs"]),
                ..good()
            },
            "uncommitted: src/sync.rs",
        ),
        (
            CheckFacts {
                baseline_before: Some(1),
                baseline_after: Some(1),
                ..good()
            },
            "failed before the change and still fails",
        ),
        (
            CheckFacts {
                review: Some(ReviewFacts {
                    answer: Some(ReviewAnswer::Concern),
                    concern: Some("it skips the retry path".into()),
                    ..Default::default()
                }),
                ..good()
            },
            "it skips the retry path",
        ),
        (
            CheckFacts {
                diff: diff(&["src/sync.rs"], 400),
                ..good()
            },
            "no independent review",
        ),
    ];
    for (facts, want) in cases {
        let j = judged(facts);
        assert_eq!(j.verdict, Verdict::NeedsYou, "{want}");
        assert!(concern(&j).contains(want), "{}", concern(&j));
    }
    let unscoped = verdict(
        &good(),
        &WorkerSpec {
            scope_paths: Vec::new(),
            ..spec()
        },
    );
    assert!(concern(&unscoped).contains("named no files in scope"));
}

#[test]
fn an_untracked_file_alone_stays_clean_and_is_named() {
    let j = judged(CheckFacts {
        untracked: strings(&["target/cache.bin"]),
        ..good()
    });
    assert_eq!(j.verdict, Verdict::Clean);
    assert_eq!(j.concern, None);
    assert!(j.summary.contains("fix the flaky sync test"));
    assert!(j.summary.contains("target/cache.bin"));
    assert_eq!(judged(good()).verdict, Verdict::Clean);
}

#[test]
fn a_reviewer_answer_is_read_from_its_json() {
    let ok = serde_json::json!({"verdict": "approve"});
    assert_eq!(parse_review(&ok), Some((ReviewAnswer::Approve, None)));
    let worry = serde_json::json!({"verdict": "concern", "concern": "x"});
    assert_eq!(
        parse_review(&worry),
        Some((ReviewAnswer::Concern, Some("x".into())))
    );
    assert_eq!(parse_review(&serde_json::json!({"verdict": "maybe"})), None);
}
