//! Tests: a config reload applies what the agent can take at once and refuses a broken file
//! without losing the settings it runs with.

use idlemax_core::config::{Config, GuardPolicy, ReloadReport};
use idlemax_core::quota::QuotaConfig;
use idlemax_core::Paths;
use tokio::sync::{oneshot, watch};

use super::*;
use crate::agent::{metrics::Sampler, ring::MetricsRing};

const LAPTOP: &str = "[agent]\nprofile = \"laptop\"\n";

fn agent_at(paths: &Paths) -> Agent {
    let config = Config::parse(LAPTOP).unwrap();
    let probes = Probes::new(
        Sampler::new().unwrap(),
        MetricsRing::open(&paths.agent_state_dir(), 30).unwrap(),
        paths,
        QuotaConfig::default(),
        config.agent.clone(),
    );
    let (status, _status_rx) = watch::channel(LocalStatus::default());
    Agent::new(Wiring {
        memory_cap: idlemax_core::config::MemoryCap::Unlimited,
        device: DeviceInfo {
            id: "reload".into(),
            cpu_count: 4,
            mem_total_bytes: 8 << 30,
            ..DeviceInfo::default()
        },
        guard: Guard::new(GuardPolicy::laptop()),
        client: CoordClient::new("http://127.0.0.1:9", "test").unwrap(),
        probes,
        runner: Runner::new(paths.clone(), "reload".into(), config.agent.clone()),
        outbox: DurableOutbox::open(&paths.agent_state_dir(), 100, now_ms()),
        pause_file: paths.pause_file(),
        status,
        sample_every: Duration::from_secs(60),
        heartbeat_every: Duration::from_secs(60),
        advisor_inputs_every: None,
        dispatch_inputs_every: None,
        device_refresh_every: Duration::from_secs(3600),
        notify: false,
        paths: paths.clone(),
        config,
        worker_room: idlemax_core::dispatch::room::WorkerRoom::new(
            idlemax_core::dispatch::room::RoomPolicy::new(
                &idlemax_core::config::DispatcherConfig::default(),
                &idlemax_core::config::GuardPolicy::macos_laptop(),
            ),
        ),
        live: std::sync::Arc::new(crate::live_config::LiveConfig::new(
            paths,
            idlemax_core::config::Config::default(),
        )),
        workers: None,
    })
}

async fn reload(agent: &mut Agent) -> Result<ReloadReport, String> {
    let (reply, answer) = oneshot::channel();
    agent.on_reload(ReloadRequest { reply }).await;
    answer.await.unwrap()
}

#[tokio::test]
async fn a_reload_applies_the_guard_rule_and_a_broken_file_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path());
    crate::state::ensure_dirs(&paths).unwrap();
    let mut agent = agent_at(&paths);
    agent.memory_cap = idlemax_core::config::MemoryCap::Limited(640 << 20);
    assert!(agent.guard.policy().require_ac_power);

    // The file the user edited: run on battery too, and one program allowed, and a setting
    // that is read only at start.
    let edited = format!("{LAPTOP}sample_secs = 10\ncommand_allowlist = [\"git\"]\n[guard]\nrequire_ac_power = false\n");
    std::fs::write(paths.config_file(), edited).unwrap();
    let report = reload(&mut agent).await.unwrap();
    assert_eq!(
        report.applied,
        ["agent.command_allowlist", "guard.require_ac_power"]
    );
    assert_eq!(report.restart_agent, ["agent.sample_secs"]);
    assert!(!agent.guard.policy().require_ac_power);
    assert_eq!(agent.guard.policy().ram_budget_mib, 512);
    assert_eq!(
        agent.decision.memory_limit.as_ref().unwrap().effective_mib,
        512
    );

    // A file that does not parse is refused and the settings stay as they were.
    std::fs::write(paths.config_file(), "[guard\n").unwrap();
    let why = reload(&mut agent).await.unwrap_err();
    assert!(why.contains("config.toml"), "{why}");
    assert!(!agent.guard.policy().require_ac_power);

    // Nothing changed since the last good reload: nothing to report.
    std::fs::write(paths.config_file(), format!("{LAPTOP}sample_secs = 10\ncommand_allowlist = [\"git\"]\n[guard]\nrequire_ac_power = false\n")).unwrap();
    assert!(reload(&mut agent).await.unwrap().is_empty());
}
