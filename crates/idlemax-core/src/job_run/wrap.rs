//! Domain: the OS throttle argv wrapped around a job (taskpolicy, systemd scope, nice).

use crate::types::{JobId, UnixMs};

/// OS throttle wrapped around a job's argv. Tool paths come from the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrapper {
    /// macOS: background QoS, throttled disk I/O, PRIO_DARWIN_BG, memory limit, nice 20.
    Taskpolicy { taskpolicy: String, nice: String },
    /// Linux: transient systemd scope with cgroup v2 caps; `user` adds `--user`.
    SystemdScope { systemd_run: String, user: bool },
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

    /// Full argv: wrapper prefix, then the job argv unchanged.
    pub fn wrap(&self, argv: &[String], mem_mb: u64, unit: &str) -> Vec<String> {
        let mut out = match self {
            Wrapper::Taskpolicy { taskpolicy, nice } => {
                let mut v = strings(&[taskpolicy, "-c", "background", "-d", "throttle", "-b"]);
                v.extend(["-m".into(), mem_mb.to_string(), "-P".into(), "kill".into()]);
                v.extend(strings(&[nice, "-n", "20"]));
                v
            }
            Wrapper::SystemdScope { systemd_run, user } => {
                systemd_prefix(systemd_run, *user, mem_mb, unit)
            }
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

fn systemd_prefix(systemd_run: &str, user: bool, mem_mb: u64, unit: &str) -> Vec<String> {
    let mut v = vec![systemd_run.to_string()];
    if user {
        v.push("--user".into());
    }
    v.extend(strings(&["--scope", "--quiet", "--collect"]));
    v.push(format!("--unit={unit}"));
    v.push("--nice=19".into());
    let high = (mem_mb * 3 / 4).max(1);
    for prop in [
        "CPUQuota=100%".to_string(),
        "CPUWeight=10".into(),
        "IOWeight=10".into(),
        format!("MemoryHigh={high}M"),
        format!("MemoryMax={mem_mb}M"),
        "MemorySwapMax=0".into(),
    ] {
        v.push("-p".into());
        v.push(prop);
    }
    v.push("--".into());
    v
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Unique transient unit name for one attempt.
pub fn scope_unit(id: JobId, started_ms: UnixMs) -> String {
    format!("idlemax-job-{id}-{started_ms}")
}
