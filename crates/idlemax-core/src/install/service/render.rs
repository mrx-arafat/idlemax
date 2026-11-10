//! Domain: Linux unit rendering and capacity-derived service limits. Pure.

use super::*;

/// Offline root-unit rendering, usable even on a non-Linux build host.
pub fn render_system(kind: ServiceKind, env: &ServiceEnv) -> Result<UnitFile, ServiceError> {
    // Reuse validation, quoting and environment handling from the user-unit renderer.
    render(Platform::SystemdUser, kind, env)?;
    let template = match kind {
        ServiceKind::Agent => include_str!("../../../../../packaging/linux/idlemax-agent.service"),
        ServiceKind::Coord => include_str!("../../../../../packaging/linux/idlemax-coord.service"),
    };
    let mut content = template.replace(
        "ExecStart=/usr/local/bin/idlemax",
        &format!(
            "ExecStart={}",
            systemd_exec_word(text("binary path", &env.bin)?)
        ),
    );
    let mut environment = vec![systemd_env_line(
        "IDLEMAX_HOME",
        text("state directory", &env.state_dir)?,
    )];
    environment.extend(
        env_rows(kind, env)
            .into_iter()
            .filter(|(name, _)| *name != "IDLEMAX_HOME")
            .map(|(name, value)| systemd_env_line(name, value)),
    );
    content = content.replace(
        "Environment=IDLEMAX_HOME=/var/lib/idlemax",
        &environment.join("\n"),
    );
    // The root template's write sandbox must follow its configured state directory too.
    content = content.replace(
        "ProtectSystem=strict",
        &format!(
            "ProtectSystem=strict\nReadWritePaths={}",
            systemd_exec_word(text("state directory", &env.state_dir)?).replace("$$", "$")
        ),
    );
    Ok(UnitFile {
        path: PathBuf::from("/etc/systemd/system").join(kind.unit()),
        content: sized_unit(content, kind, env),
    })
}

pub(super) fn sized_unit(content: String, kind: ServiceKind, env: &ServiceEnv) -> String {
    if kind != ServiceKind::Agent {
        return content;
    }
    let limits = [
        (
            "MemoryHigh",
            format!(
                "{}M",
                env.agent_ram_mib
                    .saturating_add(crate::config::AGENT_OVERHEAD_MIB / 2)
            ),
        ),
        (
            "MemoryMax",
            format!(
                "{}M",
                env.agent_ram_mib
                    .saturating_add(crate::config::AGENT_OVERHEAD_MIB)
            ),
        ),
        (
            "CPUQuota",
            format!("{}%", u64::from(env.agent_jobs.max(1)) * 100),
        ),
    ];
    content
        .lines()
        .map(|line| {
            limits
                .iter()
                .find(|(key, _)| line.starts_with(&format!("{key}=")))
                .map_or_else(|| line.to_string(), |(key, value)| format!("{key}={value}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
