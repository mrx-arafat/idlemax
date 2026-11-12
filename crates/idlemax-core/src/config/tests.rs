//! Tests: parsing, validating and sizing `config.toml`.

use super::*;

#[test]
fn a_token_must_be_long_enough_to_resist_guessing() {
    assert!(matches!(parse_token(" \n"), Err(ConfigError::EmptyToken)));
    assert!(matches!(
        parse_token("a\n"),
        Err(ConfigError::WeakToken { len: 1 })
    ));
    let short = "x".repeat(MIN_TOKEN_CHARS - 1);
    assert!(matches!(
        parse_token(&short),
        Err(ConfigError::WeakToken { len }) if len == MIN_TOKEN_CHARS - 1
    ));
    let enough = "x".repeat(MIN_TOKEN_CHARS);
    assert_eq!(parse_token(&format!("  {enough}\n")).unwrap(), enough);
    // What `idlemax init` generates is accepted.
    assert!(parse_token(&encode_token(&[0xab; 32])).is_ok());
}

#[test]
fn a_laptop_takes_half_the_cpus_and_a_quarter_of_the_ram_unless_the_config_sets_them() {
    const GIB: u64 = 1 << 30;
    let slots_and_ram = |text: &str, cpus: u32, ram: u64| {
        let config = Config::parse(text).unwrap();
        let p = config.guard_policy_for(Os::Macos, cpus, ram).unwrap();
        (p.max_jobs, p.ram_budget_mib)
    };
    let laptop = "[agent]\nprofile = \"macos_laptop\"\n";
    assert_eq!(slots_and_ram(laptop, 10, 16 * GIB), (5, 4096));
    // A small machine keeps the preset: 2 jobs, 1 GiB.
    assert_eq!(slots_and_ram(laptop, 2, 2 * GIB), (2, 1024));
    let pinned = format!("{laptop}[guard]\nmax_jobs = 1\nram_budget_mib = 256\n");
    assert_eq!(slots_and_ram(&pinned, 10, 16 * GIB), (1, 256));
}

#[test]
fn server_capacity_reserves_resources_and_honours_explicit_overrides() {
    let server = "[agent]\nprofile = \"linux_server\"\n";
    let cfg = Config::parse(server).unwrap();
    for (cpus, gib, jobs, ram) in [
        (2, 2, 1, 512),
        (4, 4, 2, 1024),
        (8, 8, 4, 2048),
        (16, 16, 8, 4096),
    ] {
        let policy = cfg.guard_policy_for(Os::Linux, cpus, gib << 30).unwrap();
        assert_eq!((policy.max_jobs, policy.ram_budget_mib), (jobs, ram));
    }
    let small = cfg.guard_policy_for(Os::Linux, 4, 1950 << 20).unwrap();
    assert_eq!((small.max_jobs, small.ram_budget_mib), (1, 512));
    let cfg = Config::parse(&format!(
        "{server}[guard]\nmax_jobs = 1\nram_budget_mib = 256\n"
    ))
    .unwrap();
    let policy = cfg.guard_policy_for(Os::Linux, 16, 16 << 30).unwrap();
    assert_eq!((policy.max_jobs, policy.ram_budget_mib), (1, 256));
}

#[test]
fn cgroup_memory_clamps_even_pinned_budgets_and_reserves_agent_overhead() {
    let mut unknown = Config::default()
        .guard_policy_for(Os::Linux, 16, 16 << 30)
        .unwrap();
    let note = unknown.apply_memory_cap(MemoryCap::Unknown, true).unwrap();
    assert_eq!((unknown.max_jobs, unknown.ram_budget_mib), (1, 512));
    assert!(note.to_string().contains("reference fallback"));
    assert!(note.cgroup_max_bytes.is_none());
    for (cap, budget, expected) in [
        (None, 4096, 4096),
        (Some(640 << 20), 4096, 512),
        (Some(8192 << 20), 4096, 4096),
        (Some(127 << 20), 512, 0),
        (Some((640 << 20) + 1), 4096, 512),
    ] {
        let mut policy = GuardPolicy::production();
        policy.ram_budget_mib = budget;
        let note = policy.clamp_memory(cap);
        assert_eq!(policy.ram_budget_mib, expected);
        assert_eq!(note.is_some(), budget != expected);
    }
}

#[test]
fn agent_config_notify_flag() {
    let default_cfg: Config = toml::from_str("").unwrap();
    assert!(!default_cfg.agent.notify);

    let custom_cfg: Config = toml::from_str("[agent]\nnotify = true\n").unwrap();
    assert!(custom_cfg.agent.notify);
}

#[test]
fn a_bad_guard_value_is_one_line_naming_its_key() {
    let err = Config::parse("[guard]\nswap_growth_kill = 100000000000\n").unwrap_err();
    let msg = err.to_string();
    assert_eq!(msg.lines().count(), 1, "{msg:?}");
    assert!(
        msg.starts_with("guard.swap_growth_kill: invalid type"),
        "{msg:?}"
    );
}

#[test]
fn profiles_parse_aliases_defaults_and_sizing() {
    const GIB: u64 = 1 << 30;
    let linux_old = Config::parse("[agent]\nprofile = \"linux_server\"\n").unwrap();
    assert_eq!(linux_old.agent.profile, Some(DeviceProfile::Production));
    let mac_old = Config::parse("[agent]\nprofile = \"macos_laptop\"\n").unwrap();
    assert_eq!(mac_old.agent.profile, Some(DeviceProfile::Laptop));
    let prod_new = Config::parse("[agent]\nprofile = \"production\"\n").unwrap();
    assert_eq!(prod_new.agent.profile, Some(DeviceProfile::Production));
    let laptop_new = Config::parse("[agent]\nprofile = \"laptop\"\n").unwrap();
    assert_eq!(laptop_new.agent.profile, Some(DeviceProfile::Laptop));
    let workbox = Config::parse("[agent]\nprofile = \"workbox\"\n").unwrap();
    assert_eq!(workbox.agent.profile, Some(DeviceProfile::Workbox));

    assert!(prod_new
        .to_toml_string()
        .unwrap()
        .contains("profile = \"production\""));
    assert!(laptop_new
        .to_toml_string()
        .unwrap()
        .contains("profile = \"laptop\""));
    assert!(workbox
        .to_toml_string()
        .unwrap()
        .contains("profile = \"workbox\""));

    assert_eq!(
        DeviceProfile::default_for(Os::Linux),
        DeviceProfile::Production
    );
    assert_eq!(DeviceProfile::default_for(Os::Macos), DeviceProfile::Laptop);

    let workbox_policy = workbox.guard_policy_for(Os::Linux, 8, 16 * GIB).unwrap();
    assert_eq!(
        (workbox_policy.max_jobs, workbox_policy.ram_budget_mib),
        (7, 15232)
    );
    let prod_policy = prod_new.guard_policy_for(Os::Linux, 8, 16 * GIB).unwrap();
    assert_eq!(
        (prod_policy.max_jobs, prod_policy.ram_budget_mib),
        (7, 4096)
    );
    let small_workbox = workbox.guard_policy_for(Os::Linux, 2, 2 * GIB).unwrap();
    assert_eq!(
        (small_workbox.max_jobs, small_workbox.ram_budget_mib),
        (1, 896)
    );

    let wb_preset = GuardPolicy::workbox();
    assert_eq!(wb_preset.psi_mem_some_pause, Some(10.0));
    assert_eq!(wb_preset.psi_mem_full_kill, Some(5.0));
    assert_eq!(wb_preset.load_per_cpu_pause, None);
    assert_eq!(wb_preset.load_per_cpu_kill, None);
    // The numbers `linux_server()` had before the rename.
    let old_server = GuardPolicy {
        max_jobs: 1,
        ram_budget_mib: 512,
        kill_after_breaches: 2,
        resume_after_clear: 2,
        user_idle_min_secs: None,
        require_ac_power: false,
        min_battery_pct: None,
        pause_on_thermal: true,
        memory_pressure_pause: None,
        memory_pressure_kill: None,
        mem_free_pct_pause: None,
        mem_free_pct_kill: None,
        mem_available_mib_pause: Some(1024),
        mem_available_mib_kill: Some(768),
        swap_growth_pause: None,
        swap_growth_kill: None,
        swap_used_mib_pause: None,
        swap_used_mib_kill: None,
        swap_in_pages_per_sec_pause: Some(100.0),
        swap_in_sustain_secs: 60,
        psi_mem_some_pause: Some(5.0),
        psi_mem_full_kill: Some(1.0),
        psi_io_full_pause: Some(10.0),
        psi_io_full_kill: Some(25.0),
        load_per_cpu_pause: Some(0.75),
        load_per_cpu_kill: Some(1.5),
        load_1m_pause: None,
        load_1m_kill: None,
        disk_free_mib_pause: Some(2048),
        disk_free_mib_kill: Some(1024),
    };
    assert_eq!(GuardPolicy::production(), old_server);
}
