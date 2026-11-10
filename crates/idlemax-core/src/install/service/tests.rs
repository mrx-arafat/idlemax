use super::*;

fn env() -> ServiceEnv {
    ServiceEnv {
        agent_jobs: 1,
        agent_ram_mib: 512,
        bin: "/opt/my tools/idlemax".into(),
        home: "/home/u".into(),
        state_dir: "/data & more/idlemax".into(),
        path: Some("/usr/bin:/opt/n&b/bin".into()),
        vars: vec![
            ("IDLEMAX_HOME".into(), "/data & more/idlemax".into()),
            ("CODEX_HOME".into(), "/home/u/.codex".into()),
        ],
        uid: 501,
    }
}

#[test]
fn rendered_agent_caps_follow_the_guard_budget_for_root_and_user_units() {
    let disabled = ServiceEnv {
        agent_jobs: 0,
        ..env()
    };
    assert!(render_system(ServiceKind::Agent, &disabled)
        .unwrap()
        .content
        .contains("CPUQuota=100%"));
    use crate::{types::Os, Config};
    for (text, jobs, ram, high, max) in [
        ("", 8, 4096, "MemoryHigh=4160M", "MemoryMax=4224M"),
        (
            "[guard]\nmax_jobs = 1\nram_budget_mib = 512\n",
            1,
            512,
            "MemoryHigh=576M",
            "MemoryMax=640M",
        ),
    ] {
        let policy = Config::parse(text)
            .unwrap()
            .guard_policy_for(Os::Linux, 16, 16 << 30)
            .unwrap();
        assert_eq!((policy.max_jobs, policy.ram_budget_mib), (jobs, ram));
        let env = ServiceEnv {
            agent_jobs: policy.max_jobs,
            agent_ram_mib: policy.ram_budget_mib,
            ..env()
        };
        for unit in [
            render(Platform::SystemdUser, ServiceKind::Agent, &env).unwrap(),
            render_system(ServiceKind::Agent, &env).unwrap(),
        ] {
            assert!(unit.content.contains(high), "{}", unit.content);
            assert!(unit.content.contains(max), "{}", unit.content);
            assert!(unit.content.contains(&format!("CPUQuota={}%", jobs * 100)));
            assert!(!unit.content.contains('@'));
        }
        let root = render_system(ServiceKind::Agent, &env).unwrap();
        assert_eq!(
            root.path,
            PathBuf::from("/etc/systemd/system/idlemax-agent.service")
        );
        assert!(root.content.contains("User=idlemax"));
        assert!(root
            .content
            .contains("ReadWritePaths=\"/data & more/idlemax\""));
    }
}

#[test]
fn plist_has_real_paths_escaped_and_nothing_unfilled() {
    let agent = render(Platform::Launchd, ServiceKind::Agent, &env()).unwrap();
    assert_eq!(
        agent.path,
        PathBuf::from("/home/u/Library/LaunchAgents/com.idlemax.agent.plist")
    );
    let c = &agent.content;
    assert!(c.contains("<string>/opt/my tools/idlemax</string>"), "{c}");
    assert!(c.contains("<key>IDLEMAX_HOME</key>\n\t\t<string>/data &amp; more/idlemax</string>"));
    assert!(c.contains("<string>/usr/bin:/opt/n&amp;b/bin</string>"));
    assert!(c.contains("<string>/data &amp; more/idlemax/logs/agent.log</string>"));
    assert!(!c.contains('@'), "unfilled placeholder in {c}");

    let coord = render(Platform::Launchd, ServiceKind::Coord, &env())
        .unwrap()
        .content;
    assert!(coord.contains("<key>IDLEMAX_HOME</key>"));
    assert!(
        !coord.contains("PATH") && !coord.contains("CODEX_HOME"),
        "{coord}"
    );
    let bare = ServiceEnv {
        path: None,
        vars: vec![],
        ..env()
    };
    let bare = render(Platform::Launchd, ServiceKind::Coord, &bare)
        .unwrap()
        .content;
    assert!(
        !bare.contains("EnvironmentVariables") && !bare.contains('@'),
        "{bare}"
    );
}

#[test]
fn systemd_unit_quotes_values_and_keeps_the_system_unit_caps() {
    let unit = render(Platform::SystemdUser, ServiceKind::Agent, &env()).unwrap();
    assert_eq!(
        unit.path,
        PathBuf::from("/home/u/.config/systemd/user/idlemax-agent.service")
    );
    let c = &unit.content;
    assert!(
        c.contains("ExecStart=\"/opt/my tools/idlemax\" agent\n"),
        "{c}"
    );
    assert!(c.contains("Environment=\"IDLEMAX_HOME=/data & more/idlemax\"\n"));
    assert!(c.contains("Environment=\"PATH=/usr/bin:/opt/n&b/bin\"\n"));
    assert!(!c.contains('@') && !c.contains("User=") && c.contains("WantedBy=default.target"));

    // A cap changed in the system unit must change in the user unit too.
    const CAPS: [&str; 10] = [
        "MemoryHigh",
        "MemoryMax",
        "MemorySwapMax",
        "CPUQuota",
        "CPUWeight",
        "IOWeight",
        "Nice",
        "TasksMax",
        "IOSchedulingClass",
        "TimeoutStopSec",
    ];
    for (user, system) in [
        (
            AGENT_UNIT,
            include_str!("../../../../../packaging/linux/idlemax-agent.service"),
        ),
        (
            COORD_UNIT,
            include_str!("../../../../../packaging/linux/idlemax-coord.service"),
        ),
    ] {
        let value = |text: &str, key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
                .map(str::to_string)
        };
        for key in CAPS {
            assert_eq!(value(user, key), value(system, key), "{key} drifted");
        }
    }
}

#[test]
fn bad_inputs_are_refused() {
    let relative = ServiceEnv {
        agent_jobs: 1,
        agent_ram_mib: 512,
        bin: "idlemax".into(),
        ..env()
    };
    assert_eq!(
        render(Platform::Launchd, ServiceKind::Agent, &relative),
        Err(ServiceError::RelativeBin("idlemax".into()))
    );
    let injected = ServiceEnv {
        path: Some("/bin\nExecStartPre=/x".into()),
        ..env()
    };
    assert!(render(Platform::SystemdUser, ServiceKind::Agent, &injected).is_err());
    assert!(bin_warning(Path::new("/w/target/debug/idlemax")).is_some());
    assert!(bin_warning(Path::new("/home/u/.cargo/bin/idlemax")).is_none());
}

#[test]
fn commands_per_platform() {
    let e = env();
    let show = |steps: Vec<Step>| steps.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        show(install_steps(
            Platform::Launchd,
            ServiceKind::Agent,
            &e,
            UnitChange::Update
        )),
        [
            "launchctl bootout gui/501/com.idlemax.agent  # a failure here is fine",
            "launchctl bootstrap gui/501 /home/u/Library/LaunchAgents/com.idlemax.agent.plist",
        ]
    );
    let systemd = |change| {
        show(install_steps(
            Platform::SystemdUser,
            ServiceKind::Agent,
            &e,
            change,
        ))
    };
    // A running unit keeps its old ExecStart until it restarts; an unchanged one only starts.
    assert_eq!(
        systemd(UnitChange::Update),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable idlemax-agent.service",
            "systemctl --user restart idlemax-agent.service",
        ]
    );
    assert_eq!(
        systemd(UnitChange::Same),
        [
            "systemctl --user enable idlemax-agent.service",
            "systemctl --user start idlemax-agent.service",
        ]
    );
    let r = removal(Platform::SystemdUser, ServiceKind::Coord, &e);
    assert_eq!(
        show(r.before),
        ["systemctl --user disable --now idlemax-coord.service"]
    );
    assert_eq!(show(r.after), ["systemctl --user daemon-reload"]);
    assert_eq!(
        status_step(Platform::Launchd, ServiceKind::Coord, &e).to_string(),
        "launchctl print gui/501/com.idlemax.coord"
    );
}

#[test]
fn status_reads_each_state() {
    let f = || PathBuf::from("/f");
    let st = |p, probe| parse_status(p, ServiceKind::Agent, f(), probe);
    let ran = |ok, stdout, stderr| Probe::Ran { ok, stdout, stderr };
    let (launchd, systemd) = (Platform::Launchd, Platform::SystemdUser);

    let gone = st(launchd, Probe::NotInstalled);
    assert_eq!(
        (gone.installed, gone.loaded, gone.running),
        (false, Some(false), Some(false))
    );
    let up = st(
        launchd,
        ran(true, "gui/501/x = {\n\tstate = running\n\tpid = 7\n}", ""),
    );
    assert_eq!(
        (up.installed, up.loaded, up.running),
        (true, Some(true), Some(true))
    );
    let idle = st(launchd, ran(true, "x = {\n\tstate = not running\n}", ""));
    assert_eq!((idle.loaded, idle.running), (Some(true), Some(false)));
    let unloaded = st(launchd, ran(false, "", "Could not find service"));
    assert_eq!(
        (unloaded.installed, unloaded.loaded, unloaded.running),
        (true, Some(false), Some(false))
    );

    let up = st(
        systemd,
        ran(true, "ActiveState=active\nLoadState=loaded\n", ""),
    );
    assert_eq!((up.loaded, up.running), (Some(true), Some(true)));
    let stopped = st(
        systemd,
        ran(true, "LoadState=loaded\nActiveState=inactive\n", ""),
    );
    assert_eq!((stopped.loaded, stopped.running), (Some(true), Some(false)));
    let unknown = st(
        systemd,
        ran(true, "LoadState=not-found\nActiveState=inactive\n", ""),
    );
    assert_eq!(
        (unknown.loaded, unknown.running),
        (Some(false), Some(false))
    );
    let no_bus = st(
        systemd,
        ran(false, "", "Failed to connect to bus: No medium found\nmore"),
    );
    assert_eq!((no_bus.loaded, no_bus.running), (None, None));
    assert_eq!(
        no_bus.note.as_deref(),
        Some("systemctl --user failed: Failed to connect to bus: No medium found")
    );
    assert!(st(launchd, Probe::Missing("launchctl"))
        .note
        .unwrap()
        .contains("launchctl not found"));
}
