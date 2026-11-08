use super::*;

const TAILNET: &str = "http://100.64.0.10:7878";

fn request(role: Role) -> Request<'static> {
    Request {
        role,
        os: Os::Linux,
        coord_url: None,
        token_given: false,
        service: true,
        statusline: false,
    }
}

const NOTHING: Existing<'static> = Existing {
    config: None,
    token: false,
};

fn written(plan: &SetupPlan) -> &Config {
    match &plan.config {
        ConfigPlan::Write(cfg) => cfg,
        ConfigPlan::Keep => panic!("expected a new config.toml"),
    }
}

#[test]
fn both_is_a_complete_single_machine_setup_with_the_coordinator_started_first() {
    let plan = plan(&request(Role::Both), &NOTHING).unwrap();
    let cfg = written(&plan);
    assert_eq!(cfg.coordinator.url, "http://127.0.0.1:7878");
    assert_eq!(cfg.coordinator.listen, "127.0.0.1:7878");
    assert_eq!(cfg.agent.profile, Some(DeviceProfile::LinuxServer));
    assert_eq!(plan.token, TokenPlan::Generate);
    assert_eq!(plan.services, [ServiceKind::Coord, ServiceKind::Agent]);
}

#[test]
fn coord_listens_where_agents_are_told_to_connect() {
    let req = Request {
        coord_url: Some("http://100.64.0.10:7878/"),
        ..request(Role::Coord)
    };
    let plan = plan(&req, &NOTHING).unwrap();
    let cfg = written(&plan);
    assert_eq!(cfg.coordinator.url, TAILNET);
    assert_eq!(cfg.coordinator.listen, "100.64.0.10:7878");
    assert_eq!(plan.services, [ServiceKind::Coord]);
    assert_eq!(plan.token, TokenPlan::Generate);
}

#[test]
fn agent_reports_to_a_remote_coordinator_with_its_token_and_only_runs_the_agent_service() {
    let req = Request {
        coord_url: Some(TAILNET),
        token_given: true,
        ..request(Role::Agent)
    };
    let plan = plan(&req, &NOTHING).unwrap();
    let cfg = written(&plan);
    assert_eq!(cfg.coordinator.url, TAILNET);
    assert_eq!(
        cfg.coordinator.listen, "127.0.0.1:7878",
        "an agent serves no one"
    );
    assert_eq!(plan.token, TokenPlan::FromStdin);
    assert_eq!(plan.services, [ServiceKind::Agent]);

    let no_url = plan_err(Request {
        coord_url: None,
        ..req
    });
    assert_eq!(no_url, SetupError::NeedsCoordUrl);
    let loopback = plan_err(Request {
        coord_url: Some("http://localhost:7878"),
        ..req
    });
    assert_eq!(loopback, SetupError::NeedsCoordUrl);
    let no_token = plan_err(Request {
        token_given: false,
        ..req
    });
    assert_eq!(no_token, SetupError::NeedsToken);
    let bad = plan_err(Request {
        coord_url: Some("100.64.0.10:7878"),
        ..req
    });
    assert!(matches!(bad, SetupError::BadUrl(_)));
}

fn plan_err(req: Request<'_>) -> SetupError {
    plan(&req, &NOTHING).unwrap_err()
}

#[test]
fn a_finished_setup_plans_nothing_to_write_and_refuses_a_different_address() {
    let cfg = config_for(Role::Both, Os::Linux, Some(TAILNET));
    let existing = Existing {
        config: Some(&cfg),
        token: true,
    };
    let same = Request {
        coord_url: Some(TAILNET),
        token_given: true,
        ..request(Role::Both)
    };
    let plan = plan(&same, &existing).unwrap();
    assert!(matches!(plan.config, ConfigPlan::Keep));
    assert_eq!(plan.token, TokenPlan::Keep);
    assert_eq!(plan.coord_url, TAILNET);

    let other = Request {
        coord_url: Some("http://100.64.0.99:7878"),
        ..same
    };
    let err = super::plan(&other, &existing).unwrap_err();
    assert!(matches!(err, SetupError::UrlConflict { .. }), "{err}");

    // An agent whose existing config points at itself still has nowhere to report.
    let local = Config::default();
    let agent = super::plan(
        &request(Role::Agent),
        &Existing {
            config: Some(&local),
            token: true,
        },
    );
    assert_eq!(agent.unwrap_err(), SetupError::NeedsCoordUrl);
}

#[test]
fn services_and_statusline_are_skipped_with_the_reason() {
    let no_service = Request {
        service: false,
        statusline: true,
        ..request(Role::Coord)
    };
    let plan = plan(&no_service, &NOTHING).unwrap();
    assert!(plan.services.is_empty());
    assert_eq!(plan.service_skipped, Some("--no-service was given"));
    assert!(matches!(plan.statusline, StatuslinePlan::Skip(r) if r.contains("Claude Code")));

    let windows = Request {
        os: Os::Windows,
        statusline: true,
        ..request(Role::Both)
    };
    let plan = super::plan(&windows, &NOTHING).unwrap();
    assert!(plan.services.is_empty() && plan.service_skipped.is_some());
    assert_eq!(plan.statusline, StatuslinePlan::Install);
}

#[test]
fn a_unit_file_is_only_rewritten_when_its_content_changes() {
    assert_eq!(unit_change(None, "a"), UnitChange::Create);
    assert_eq!(unit_change(Some("a"), "a"), UnitChange::Same);
    assert_eq!(unit_change(Some("old"), "a"), UnitChange::Update);
}
