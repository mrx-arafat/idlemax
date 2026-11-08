//! Domain: the OS throttle argv wrapped around a job (taskpolicy, systemd scope, nice).

use crate::types::{JobId, UnixMs};

/// OS throttle wrapped around a job's argv. Tool paths come from the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrapper {
    /// macOS: background QoS, throttled disk I/O, PRIO_DARWIN_BG, memory limit, nice 20.
    Taskpolicy { taskpolicy: String, nice: String },
    /// Linux: transient systemd scope with cgroup v2 caps; `user` adds `--user`.
    /// `systemd` is the major version of `systemd-run` (0 when unknown) and picks the
    /// switches it understands, see `systemd_prefix`.
    SystemdScope {
        systemd_run: String,
        user: bool,
        systemd: u32,
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
                systemd,
            } => systemd_prefix(systemd_run, *user, *systemd, mem_mb, unit),
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
/// From systemd 254 `--expand-environment=no` keeps the job argv as given; 258 made `${VAR}`
/// expansion the default for scopes. From 253 `OOMPolicy=kill` SIGKILLs the whole job on a
/// cap breach, whatever process the kernel picked. An unknown version gets neither switch.
fn systemd_prefix(
    systemd_run: &str,
    user: bool,
    systemd: u32,
    mem_mb: u64,
    unit: &str,
) -> Vec<String> {
    let mut v = vec![systemd_run.to_string()];
    if user {
        v.push("--user".into());
    }
    v.extend(strings(&["--scope", "--quiet", "--collect"]));
    if systemd >= EXPAND_ENVIRONMENT_SINCE {
        v.push("--expand-environment=no".into());
    }
    v.push(format!("--unit={unit}"));
    v.push("--nice=19".into());
    let mut props = vec![
        "CPUQuota=100%".to_string(),
        "CPUWeight=10".into(),
        "IOWeight=10".into(),
        format!("MemoryMax={mem_mb}M"),
        "MemorySwapMax=0".into(),
    ];
    if systemd >= SCOPE_OOM_POLICY_SINCE {
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
/// First systemd release whose `systemd-run` has `--expand-environment=` (its NEWS for v254).
const EXPAND_ENVIRONMENT_SINCE: u32 = 254;

/// Reads the major version from `systemd-run --version` output
/// ("systemd 255 (255.4-1ubuntu8.17)"). None when the line is missing or malformed.
pub fn systemd_major(version_output: &str) -> Option<u32> {
    version_output
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("systemd "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|major| major.parse::<u32>().ok())
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

    fn scope(systemd: u32) -> Wrapper {
        Wrapper::SystemdScope {
            systemd_run: "/usr/bin/systemd-run".into(),
            user: true,
            systemd,
        }
    }

    #[test]
    fn systemd_scope_caps_memory_hard_without_a_throttle_below_the_cap() {
        let full = scope(255).wrap(&argv(), 128, "idlemax-job-1-2");
        let has = |p: &str| full.iter().any(|a| a == p);
        assert!(has("MemoryMax=128M") && has("MemorySwapMax=0"), "{full:?}");
        assert!(
            !full.iter().any(|a| a.starts_with("MemoryHigh")),
            "{full:?}"
        );
        assert!(has("OOMPolicy=kill"), "{full:?}");
        assert_eq!(full.last().map(String::as_str), Some("/bin/job"));

        let old = scope(252).wrap(&argv(), 128, "idlemax-job-1-2");
        assert!(!old.iter().any(|a| a.starts_with("OOMPolicy")), "{old:?}");
    }

    /// systemd 258 turned `${VAR}` expansion on for scopes, which rewrote job arguments.
    /// The switch that stops it exists from 254; an older or unknown systemd must not get it.
    #[test]
    fn systemd_never_expands_variables_in_job_arguments() {
        let job = vec!["/bin/echo".to_string(), "${USER} costs $$5".to_string()];
        let new = scope(258).wrap(&job, 128, "u");
        let sep = new.iter().position(|a| a == "--").unwrap();
        assert!(
            new[..sep].iter().any(|a| a == "--expand-environment=no"),
            "{new:?}"
        );
        assert_eq!(&new[sep + 1..], job.as_slice());
        assert!(scope(254)
            .wrap(&job, 128, "u")
            .contains(&"--expand-environment=no".to_string()));
        for old in [253, 0] {
            let full = scope(old).wrap(&job, 128, "u");
            assert!(
                !full.iter().any(|a| a.starts_with("--expand-environment")),
                "{old}: {full:?}"
            );
        }
    }

    #[test]
    fn systemd_major_reads_the_version_line_or_gives_none() {
        let cases = [
            ("systemd 258 (258.1-1)\n+PAM +AUDIT", Some(258)),
            ("systemd 255 (255.4-1ubuntu8.17)\n+PAM +AUDIT", Some(255)),
            ("systemd 252 (252.22-1~deb12u1)", Some(252)),
            ("", None),
            ("not systemd at all", None),
        ];
        for (out, want) in cases {
            assert_eq!(systemd_major(out), want, "{out:?}");
        }
    }

    #[test]
    fn only_the_systemd_scope_enforces_a_hard_memory_cap() {
        assert!(scope(0).enforces_memory_cap());
        assert!(!Wrapper::Direct.enforces_memory_cap());
        let mac = Wrapper::Taskpolicy {
            taskpolicy: "t".into(),
            nice: "n".into(),
        };
        assert!(!mac.enforces_memory_cap());
    }
}
