//! Domain: the OS throttle argv wrapped around a job (taskpolicy, systemd scope, nice).

use crate::types::{JobId, UnixMs};

/// OS throttle wrapped around a job's argv. Tool paths come from the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrapper {
    /// macOS: background QoS, throttled disk I/O, PRIO_DARWIN_BG, memory limit, nice 20.
    Taskpolicy { taskpolicy: String, nice: String },
    /// Linux: transient systemd scope with cgroup v2 caps; `user` adds `--user`.
    /// `oom_kill` adds `OOMPolicy=kill` (systemd 253+) so a cap breach SIGKILLs the whole
    /// job, whatever process the kernel picked.
    SystemdScope {
        systemd_run: String,
        user: bool,
        oom_kill: bool,
    },
    /// No cgroups: nice 19 plus the idle I/O class when ionice exists.
    Nice {
        nice: String,
        ionice: Option<String>,
    },
    /// No argv wrapper (Windows sets the idle priority class at spawn).
    Direct,
}

impl Wrapper {
    pub fn name(&self) -> &'static str {
        match self {
            Wrapper::Taskpolicy { .. } => "taskpolicy",
            Wrapper::SystemdScope { .. } => "systemd-run scope",
            Wrapper::Nice { .. } => "nice",
            Wrapper::Direct => "direct",
        }
    }

    /// True when going over `mem_mb` makes the OS kill the job. The macOS limit is soft.
    pub fn enforces_memory_cap(&self) -> bool {
        matches!(self, Wrapper::SystemdScope { .. })
    }

    /// Full argv: wrapper prefix, then the job argv unchanged.
    pub fn wrap(&self, argv: &[String], mem_mb: u64, unit: &str) -> Vec<String> {
        let mut out = match self {
            Wrapper::Taskpolicy { taskpolicy, nice } => {
                let mut v = strings(&[taskpolicy, "-c", "background", "-d", "throttle", "-b"]);
                v.extend(["-m".into(), mem_mb.to_string(), "-P".into(), "kill".into()]);
                v.extend(strings(&[nice, "-n", "20"]));
                v
            }
            Wrapper::SystemdScope {
                systemd_run,
                user,
                oom_kill,
            } => systemd_prefix(systemd_run, *user, *oom_kill, mem_mb, unit),
            Wrapper::Nice { nice, ionice } => {
                let mut v = strings(&[nice, "-n", "19"]);
                if let Some(ionice) = ionice {
                    v.extend(strings(&[ionice, "-c3"]));
                }
                v
            }
            Wrapper::Direct => Vec::new(),
        };
        out.extend(argv.iter().cloned());
        out
    }
}

/// No `MemoryHigh`: with swap off the kernel would throttle a hog below `MemoryMax` and
/// never kill it, so the job would sit stalled until its runtime limit.
fn systemd_prefix(
    systemd_run: &str,
    user: bool,
    oom_kill: bool,
    mem_mb: u64,
    unit: &str,
) -> Vec<String> {
    let mut v = vec![systemd_run.to_string()];
    if user {
        v.push("--user".into());
    }
    v.extend(strings(&["--scope", "--quiet", "--collect"]));
    v.push(format!("--unit={unit}"));
    v.push("--nice=19".into());
    let mut props = vec![
        "CPUQuota=100%".to_string(),
        "CPUWeight=10".into(),
        "IOWeight=10".into(),
        format!("MemoryMax={mem_mb}M"),
        "MemorySwapMax=0".into(),
    ];
    if oom_kill {
        props.push("OOMPolicy=kill".into());
    }
    for prop in props {
        v.push("-p".into());
        v.push(prop);
    }
    v.push("--".into());
    v
}

/// First systemd release whose scope units accept `OOMPolicy=` (its NEWS for v253).
const SCOPE_OOM_POLICY_SINCE: u32 = 253;

/// Reads `systemd-run --version` output ("systemd 255 (255.4-1ubuntu8.17)"). False when the
/// number is missing or older, so an unknown host never gets a property it may reject.
pub fn supports_scope_oom_policy(version_output: &str) -> bool {
    version_output
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("systemd "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= SCOPE_OOM_POLICY_SINCE)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Unique transient unit name for one attempt.
pub fn scope_unit(id: JobId, started_ms: UnixMs) -> String {
    format!("idlemax-job-{id}-{started_ms}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv() -> Vec<String> {
        vec!["/bin/job".into()]
    }

    fn scope(oom_kill: bool) -> Wrapper {
        Wrapper::SystemdScope {
            systemd_run: "/usr/bin/systemd-run".into(),
            user: true,
            oom_kill,
        }
    }

    #[test]
    fn systemd_scope_caps_memory_hard_without_a_throttle_below_the_cap() {
        let full = scope(true).wrap(&argv(), 128, "idlemax-job-1-2");
        let has = |p: &str| full.iter().any(|a| a == p);
        assert!(has("MemoryMax=128M") && has("MemorySwapMax=0"), "{full:?}");
        assert!(
            !full.iter().any(|a| a.starts_with("MemoryHigh")),
            "{full:?}"
        );
        assert!(has("OOMPolicy=kill"), "{full:?}");
        assert_eq!(full.last().map(String::as_str), Some("/bin/job"));

        let old = scope(false).wrap(&argv(), 128, "idlemax-job-1-2");
        assert!(!old.iter().any(|a| a.starts_with("OOMPolicy")), "{old:?}");
    }

    #[test]
    fn scope_oom_policy_needs_systemd_253() {
        let cases = [
            ("systemd 255 (255.4-1ubuntu8.17)\n+PAM +AUDIT", true),
            ("systemd 253 (253.5-1)", true),
            ("systemd 252 (252.22-1~deb12u1)", false),
            ("systemd 249 (249.11-0ubuntu3.12)", false),
            ("", false),
            ("not systemd at all", false),
        ];
        for (out, want) in cases {
            assert_eq!(supports_scope_oom_policy(out), want, "{out:?}");
        }
    }

    #[test]
    fn only_the_systemd_scope_enforces_a_hard_memory_cap() {
        assert!(scope(false).enforces_memory_cap());
        assert!(!Wrapper::Direct.enforces_memory_cap());
        let mac = Wrapper::Taskpolicy {
            taskpolicy: "t".into(),
            nice: "n".into(),
        };
        assert!(!mac.enforces_memory_cap());
    }
}
