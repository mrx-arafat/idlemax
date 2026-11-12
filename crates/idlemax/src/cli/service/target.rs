//! Delivery: offline root-unit rendering uses target facts and an explicit target config.

use super::*;
use idlemax_core::{types::DeviceProfile, Config};

pub(super) fn render(
    paths: &Paths,
    mut env: ServiceEnv,
    kind: ServiceKind,
    bin: Option<PathBuf>,
    facts: Option<(u32, u64)>,
    config_path: Option<PathBuf>,
    state_dir: Option<PathBuf>,
) -> Result<idlemax_core::install::service::UnitFile> {
    let source = if config_path.is_some() {
        "explicit"
    } else if facts.is_some() {
        "defaults"
    } else {
        "local"
    };
    let config = match config_path {
        Some(path) => Config::parse(
            &std::fs::read_to_string(&path)
                .with_context(|| format!("reading target config {}", path.display()))?,
        )?,
        None if facts.is_some() => Config::default(),
        None => crate::state::load_config(paths)?,
    };
    let profile = config.agent.profile_or_default(Os::Linux);
    if profile != DeviceProfile::Production && profile != DeviceProfile::Workbox {
        bail!("target config must use production or workbox; pass --cpus N --memory-mib N and optionally --config <server-config>");
    }
    let (cpus, memory_mib) = match facts {
        Some(facts) => facts,
        None if Os::current() == Os::Linux => {
            let mut sys = sysinfo::System::new().context("reading target host capacity")?;
            sys.refresh_cpu_usage(); sys.refresh_memory();
            (sys.cpus().len() as u32, sys.total_memory() / (1024 * 1024))
        }
        None => bail!("off-host render requires --cpus N --memory-mib N; optionally pass --config <server-config>"),
    };
    let bytes = memory_mib
        .checked_mul(1024 * 1024)
        .context("--memory-mib is too large")?;
    let policy = config.guard_policy_for(Os::Linux, cpus, bytes)?;
    env.agent_jobs = policy.max_jobs;
    env.agent_ram_mib = policy.ram_budget_mib;
    env.agent_tasks_max = idlemax_core::install::service::tasks_max(profile);
    env.bin = bin.unwrap_or_else(|| PathBuf::from("/usr/local/bin/idlemax"));
    if facts.is_some() {
        env.state_dir = PathBuf::from("/var/lib/idlemax");
        env.path = None;
        env.vars.clear();
    }
    if let Some(state_dir) = state_dir {
        if !state_dir.is_absolute() {
            bail!("--state-dir must be an absolute target path");
        }
        env.state_dir = state_dir;
    }
    let mut unit = idlemax_core::install::service::render_system(kind, &env)?;
    let profile_str = profile.as_str();
    unit.content = format!("# target facts: cpus={cpus} memory_mib={memory_mib} profile={profile_str} config={source}\n{}", unit.content);
    Ok(unit)
}
