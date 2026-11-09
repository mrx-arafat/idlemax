//! Tests: agent event loop device_info refresh and controls.

use std::time::Duration;

use idlemax_core::config::{AgentConfig, GuardPolicy};
use idlemax_core::guard::Guard;
use idlemax_core::protocol::LocalStatus;
use idlemax_core::quota::QuotaConfig;
use idlemax_core::types::DeviceInfo;
use idlemax_core::Paths;
use tokio::sync::{mpsc, watch};

use super::client::CoordClient;
use super::event_loop::{Agent, Wiring};
use super::metrics::Sampler;
use super::outbox::{DurableOutbox, OUTBOX_CAP};
use super::probes::Probes;
use super::ring::MetricsRing;
use super::runner::Runner;

#[tokio::test]
async fn occupied_agent_port_does_not_rewrite_the_live_outbox() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path());
    crate::state::ensure_dirs(&paths).unwrap();
    crate::state::write_token(&paths, &"t".repeat(40)).unwrap();
    let journal = paths.agent_state_dir().join("outbox.jsonl");
    let before = "{\"ack\":123}\n{\"update\":";
    std::fs::write(&journal, before).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = idlemax_core::Config::default();
    config.agent.listen = listener.local_addr().unwrap().to_string();
    config.agent.advisor_inputs_secs = 0;
    assert!(matches!(
        super::run(paths, config).await,
        Err(super::AgentError::Bind { .. })
    ));
    assert_eq!(std::fs::read_to_string(journal).unwrap(), before);
}

#[tokio::test]
async fn device_info_refreshes_on_ticker() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path());
    let ring = MetricsRing::open(&paths.agent_state_dir(), 30).unwrap();
    let cfg = AgentConfig {
        device_id: Some("refreshed-device-id".into()),
        ..AgentConfig::default()
    };
    let probes = Probes::new(
        Sampler::new().unwrap(),
        ring,
        &paths,
        QuotaConfig::default(),
        cfg.clone(),
    );
    let runner = Runner::new(paths.clone(), "refreshed-device-id".into(), cfg);
    let guard = Guard::new(GuardPolicy::macos_laptop());
    let client = CoordClient::new("http://127.0.0.1:9999", "dummy").unwrap();
    let initial_status = LocalStatus {
        device: "initial-device-id".into(),
        ..LocalStatus::default()
    };
    let (status_tx, mut status_rx) = watch::channel(initial_status);
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let (_reload_tx, reload_rx) = mpsc::unbounded_channel();

    let initial_device = DeviceInfo {
        id: "initial-device-id".into(),
        ..DeviceInfo::default()
    };

    let wiring = Wiring {
        device: initial_device,
        guard,
        client,
        probes,
        runner,
        outbox: DurableOutbox::open(&paths.agent_state_dir(), OUTBOX_CAP, 0),
        pause_file: paths.pause_file(),
        status: status_tx,
        sample_every: Duration::from_secs(60),
        heartbeat_every: Duration::from_secs(60),
        advisor_inputs_every: None,
        device_refresh_every: Duration::from_millis(50),
        notify: false,
        paths: paths.clone(),
        config: idlemax_core::config::Config::default(),
    };

    let agent = Agent::new(wiring);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let agent_task = tokio::spawn(async move {
        agent
            .run(
                async move {
                    let _ = shutdown_rx.await;
                },
                control_rx,
                reload_rx,
            )
            .await;
    });

    // Initial publish has "initial-device-id"
    assert_eq!(status_rx.borrow().device, "initial-device-id");

    // Wait for the hourly (50ms in test) ticker to refresh device_info
    tokio::time::timeout(Duration::from_secs(5), async {
        status_rx
            .wait_for(|s| s.device == "refreshed-device-id")
            .await
            .unwrap();
    })
    .await
    .expect("device info was not refreshed within timeout");

    let _ = shutdown_tx.send(());
    let _ = agent_task.await;
}
