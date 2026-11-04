//! Example configs and job specs stay valid against the contract and consistent with each other.

use std::path::{Path, PathBuf};

use idlemax_core::types::{CostClass, JobSpec, Os};
use idlemax_core::Config;

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn config(name: &str) -> Config {
    let text = std::fs::read_to_string(examples_dir().join(name)).unwrap();
    let cfg = Config::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    cfg.quota
        .validate()
        .unwrap_or_else(|e| panic!("{name} quota: {e}"));
    cfg
}

fn job_specs() -> Vec<(String, JobSpec)> {
    let mut paths: Vec<_> = std::fs::read_dir(examples_dir().join("jobs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            let spec = toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            (p.display().to_string(), spec)
        })
        .collect()
}

#[test]
fn examples_parse_validate_and_agree() {
    let mac = config("config.toml");
    let server = config("config.server.toml");
    assert_eq!(
        mac.quota, server.quota,
        "quota accounts must match on every host"
    );
    let mac_id = mac.agent.device_id.clone().unwrap();
    let ram_budget = mac.guard_policy(Os::Macos).unwrap().ram_budget_mib;

    let specs = job_specs();
    assert_eq!(specs.len(), 5, "MVP job examples");
    for (at, spec) in &specs {
        spec.validate().unwrap_or_else(|e| panic!("{at}: {e}"));
        let argv0 = &spec.command[0];
        assert!(
            mac.agent.command_allowlist.contains(argv0),
            "{at}: {argv0} not allowlisted"
        );
        assert_eq!(
            spec.devices,
            std::slice::from_ref(&mac_id),
            "{at}: must be pinned to the Mac"
        );
        assert!(
            spec.mem_mb <= ram_budget,
            "{at}: mem_mb over the Mac RAM budget"
        );
        // Specs point at a placeholder checkout; the script must exist in this one.
        if let Some((_, rel)) = argv0.split_once("/examples/") {
            assert!(
                examples_dir().join(rel).is_file(),
                "{at}: missing script {rel}"
            );
        }
        if spec.cost == CostClass::Ai {
            let acct = spec.ai_account.as_deref().unwrap();
            assert!(
                mac.agent.ai_accounts.iter().any(|a| a == acct),
                "{at}: Mac may not spend {acct}"
            );
            assert!(
                mac.quota.account(acct).is_some(),
                "{at}: unknown account {acct}"
            );
        }
    }
}
