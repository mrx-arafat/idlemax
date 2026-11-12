use super::*;
use crate::quota::{AccountConfig, Provider, QuotaConfig, QuotaSnapshot};
use crate::types::{DeviceProfile, ProbeSource, QuotaWindowKind};

fn claude_acc(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        provider: Provider::Claude,
        ..Default::default()
    }
}

fn codex_acc(name: &str) -> AccountConfig {
    AccountConfig {
        name: name.to_string(),
        provider: Provider::Codex,
        ..Default::default()
    }
}

#[test]
fn two_devices_with_same_login_give_no_problem_and_usable_on_allows_both() {
    let cfg = QuotaConfig {
        accounts: vec![claude_acc("claude-a")],
        ..Default::default()
    };
    let login = Login::In {
        id: Some("org-123".into()),
    };
    let dev1_accs = vec!["claude-a".to_string()];
    let dev2_accs = vec!["claude-a".to_string()];
    let devs = [
        DeviceLogins {
            device: "dev1",
            profile: DeviceProfile::Production,
            accounts: &dev1_accs,
            has_claude: true,
            claude_login: Some(&login),
        },
        DeviceLogins {
            device: "dev2",
            profile: DeviceProfile::Production,
            accounts: &dev2_accs,
            has_claude: true,
            claude_login: Some(&login),
        },
    ];

    let problems = account_problems(&cfg, &devs, &[], 1000);
    assert!(problems.is_empty());
    assert!(usable_on(&problems, "dev1", "claude-a").is_ok());
    assert!(usable_on(&problems, "dev2", "claude-a").is_ok());
}

#[test]
fn split_name_refuses_both_devices() {
    let cfg = QuotaConfig {
        accounts: vec![claude_acc("claude-a")],
        ..Default::default()
    };
    let login1 = Login::In {
        id: Some("org-1".into()),
    };
    let login2 = Login::In {
        id: Some("org-2".into()),
    };
    let dev1_accs = vec!["claude-a".to_string()];
    let dev2_accs = vec!["claude-a".to_string()];
    let devs = [
        DeviceLogins {
            device: "dev1",
            profile: DeviceProfile::Production,
            accounts: &dev1_accs,
            has_claude: true,
            claude_login: Some(&login1),
        },
        DeviceLogins {
            device: "dev2",
            profile: DeviceProfile::Production,
            accounts: &dev2_accs,
            has_claude: true,
            claude_login: Some(&login2),
        },
    ];

    let problems = account_problems(&cfg, &devs, &[], 1000);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].kind, ProblemKind::SplitName);
    assert!(problems[0].devices.contains(&"dev1".to_string()));
    assert!(problems[0].devices.contains(&"dev2".to_string()));

    let err1 = usable_on(&problems, "dev1", "claude-a").unwrap_err();
    assert!(err1.contains(&problems[0].fix));
    let err2 = usable_on(&problems, "dev2", "claude-a").unwrap_err();
    assert!(err2.contains(&problems[0].fix));
}

#[test]
fn one_login_two_names_leaves_first_usable_and_refuses_second() {
    let cfg = QuotaConfig {
        accounts: vec![claude_acc("claude-a"), claude_acc("claude-b")],
        ..Default::default()
    };
    let login = Login::In {
        id: Some("org-same".into()),
    };
    let dev1_accs = vec!["claude-a".to_string()];
    let dev2_accs = vec!["claude-b".to_string()];
    let devs = [
        DeviceLogins {
            device: "dev1",
            profile: DeviceProfile::Production,
            accounts: &dev1_accs,
            has_claude: true,
            claude_login: Some(&login),
        },
        DeviceLogins {
            device: "dev2",
            profile: DeviceProfile::Production,
            accounts: &dev2_accs,
            has_claude: true,
            claude_login: Some(&login),
        },
    ];

    let problems = account_problems(&cfg, &devs, &[], 1000);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].kind, ProblemKind::OneLoginTwoNames);
    assert_eq!(problems[0].accounts, vec!["claude-a", "claude-b"]);

    assert!(usable_on(&problems, "dev1", "claude-a").is_ok());
    let err = usable_on(&problems, "dev2", "claude-b").unwrap_err();
    assert!(err.contains(&problems[0].fix));
}

#[test]
fn not_logged_in_and_login_unknown() {
    let cfg = QuotaConfig {
        accounts: vec![claude_acc("claude-a")],
        ..Default::default()
    };
    let out_login = Login::Out;
    let unk_login = Login::Unknown {
        reason: "auth check failed".into(),
    };
    let dev1_accs = vec!["claude-a".to_string()];
    let dev2_accs = vec!["claude-a".to_string()];
    let devs = [
        DeviceLogins {
            device: "dev-out",
            profile: DeviceProfile::Production,
            accounts: &dev1_accs,
            has_claude: true,
            claude_login: Some(&out_login),
        },
        DeviceLogins {
            device: "dev-unk",
            profile: DeviceProfile::Production,
            accounts: &dev2_accs,
            has_claude: true,
            claude_login: Some(&unk_login),
        },
    ];

    let problems = account_problems(&cfg, &devs, &[], 1000);
    assert_eq!(problems.len(), 2);

    let p_out = problems
        .iter()
        .find(|p| p.kind == ProblemKind::NotLoggedIn)
        .unwrap();
    assert_eq!(p_out.devices, vec!["dev-out"]);
    assert!(usable_on(&problems, "dev-out", "claude-a").is_err());

    let p_unk = problems
        .iter()
        .find(|p| p.kind == ProblemKind::LoginUnknown)
        .unwrap();
    assert_eq!(p_unk.devices, vec!["dev-unk"]);
    assert!(p_unk.message.starts_with("cannot tell whether"));
    assert!(usable_on(&problems, "dev-unk", "claude-a").is_err());

    // An unrelated device is not refused
    assert!(usable_on(&problems, "other-dev", "claude-a").is_ok());

    // A login without an id, or no reading at all, cannot be told apart either.
    let no_id = Login::In { id: None };
    for login in [Some(&no_id), None] {
        let dev = [DeviceLogins {
            device: "dev",
            profile: DeviceProfile::Production,
            accounts: &dev1_accs,
            has_claude: true,
            claude_login: login,
        }];
        let kinds: Vec<_> = account_problems(&cfg, &dev, &[], 1000)
            .into_iter()
            .map(|p| p.kind)
            .collect();
        assert_eq!(kinds, [ProblemKind::LoginUnknown], "{login:?}");
    }
}

#[test]
fn same_reading_for_codex() {
    let cfg = QuotaConfig {
        accounts: vec![codex_acc("codex-1"), codex_acc("codex-2")],
        ..Default::default()
    };
    let now = 1_000_000_000_000;
    let snap1 = QuotaSnapshot {
        account: "codex-1".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 41.0,
        resets_at_ms: Some(now + 86_400_000),
        source: ProbeSource::CodexSessions,
        at_ms: now - 1000,
    };
    let snap2_close = QuotaSnapshot {
        account: "codex-2".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 41.5,
        resets_at_ms: Some(now + 86_400_000 + 2 * 60_000), // 2 min apart
        source: ProbeSource::CodexSessions,
        at_ms: now - 2000,
    };

    let problems = account_problems(&cfg, &[], &[snap1.clone(), snap2_close], now);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].kind, ProblemKind::SameReading);
    assert!(usable_on(&problems, "any-dev", "codex-1").is_ok());
    assert!(usable_on(&problems, "any-dev", "codex-2").is_ok());

    let snap2_far = QuotaSnapshot {
        account: "codex-2".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 47.0,
        resets_at_ms: Some(now + 86_400_000 + 2 * 60_000),
        source: ProbeSource::CodexSessions,
        at_ms: now - 2000,
    };
    let problems_far = account_problems(&cfg, &[], &[snap1, snap2_far], now);
    assert!(problems_far.is_empty());
}

#[test]
fn server_only_claude_rules() {
    let cfg = QuotaConfig {
        accounts: vec![claude_acc("claude-a")],
        ..Default::default()
    };
    let login_wb = Login::In {
        id: Some("org-server".into()),
    };
    let login_diff = Login::In {
        id: Some("org-other".into()),
    };
    let accs = vec!["claude-a".to_string()];
    let empty_accs = Vec::new();

    // 1. true when listed by two workbox devices while a laptop reports another login id
    let devs1 = [
        DeviceLogins {
            device: "wb1",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
        DeviceLogins {
            device: "wb2",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
        DeviceLogins {
            device: "lap",
            profile: DeviceProfile::Laptop,
            accounts: &empty_accs,
            has_claude: true,
            claude_login: Some(&login_diff),
        },
    ];
    assert!(server_only(&cfg, "claude-a", &devs1));

    // 2. false once the laptop lists it
    let devs2 = [
        DeviceLogins {
            device: "wb1",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
        DeviceLogins {
            device: "lap",
            profile: DeviceProfile::Laptop,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_diff),
        },
    ];
    assert!(!server_only(&cfg, "claude-a", &devs2));

    // 3. false when the laptop reports the same id without listing it
    let devs3 = [
        DeviceLogins {
            device: "wb1",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
        DeviceLogins {
            device: "lap",
            profile: DeviceProfile::Laptop,
            accounts: &empty_accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
    ];
    assert!(!server_only(&cfg, "claude-a", &devs3));

    // 4. false with one OwnerMayUse warning when the laptop has claude and reports nothing
    let devs4 = [
        DeviceLogins {
            device: "wb1",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: true,
            claude_login: Some(&login_wb),
        },
        DeviceLogins {
            device: "lap",
            profile: DeviceProfile::Laptop,
            accounts: &empty_accs,
            has_claude: true,
            claude_login: None,
        },
    ];
    assert!(!server_only(&cfg, "claude-a", &devs4));
    let problems4 = account_problems(&cfg, &devs4, &[], 1000);
    assert_eq!(problems4.len(), 1);
    assert_eq!(problems4[0].kind, ProblemKind::OwnerMayUse);
    assert_eq!(problems4[0].devices, vec!["lap"]);

    // 5. false for an account nobody lists
    let devs5 = [DeviceLogins {
        device: "wb1",
        profile: DeviceProfile::Production,
        accounts: &empty_accs,
        has_claude: true,
        claude_login: Some(&login_wb),
    }];
    assert!(!server_only(&cfg, "claude-a", &devs5));
}

#[test]
fn server_only_codex_rule() {
    let cfg = QuotaConfig {
        accounts: vec![codex_acc("codex-a")],
        ..Default::default()
    };
    let accs = vec!["codex-a".to_string()];
    let empty_accs = Vec::new();
    let devs = [
        DeviceLogins {
            device: "wb1",
            profile: DeviceProfile::Production,
            accounts: &accs,
            has_claude: false,
            claude_login: None,
        },
        DeviceLogins {
            device: "lap",
            profile: DeviceProfile::Laptop,
            accounts: &empty_accs,
            has_claude: false,
            claude_login: None,
        },
    ];
    assert!(server_only(&cfg, "codex-a", &devs));
}
