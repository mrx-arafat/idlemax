//! Delivery: next-step hints for TOON replies (AXI contextual disclosure). A few complete
//! commands that follow from what was just shown; `<id>` stands for a value the reader picks.

use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, JobId, JobStatus};

/// The coordinator answers a list request with at most this many rows
/// (`page_limit` in `coord/api.rs`), so a hint may not ask for more.
pub const PAGE_LIMIT_MAX: u32 = 1000;

fn run(cmd: &str, to: &str) -> String {
    format!("Run `{cmd}` to {to}")
}

pub fn status(devices: usize, global_paused: bool) -> Vec<String> {
    let mut hints = Vec::new();
    if devices == 0 {
        hints.push(run(
            "idlemax agent",
            "register this device (run it on each device)",
        ));
    }
    if global_paused {
        hints.push(run("idlemax resume", "let jobs run again"));
    }
    hints.push(run("idlemax jobs", "list jobs"));
    hints.push(run("idlemax quota", "see AI quota headroom"));
    hints
}

pub fn jobs(shown: usize, total: Option<u64>, filter: Option<JobStatus>) -> Vec<String> {
    if shown == 0 {
        return vec![run("idlemax job add <spec.toml>", "queue a job")];
    }
    let mut hints = vec![
        run("idlemax why <id>", "see why a job is not running"),
        run("idlemax logs <id>", "read a job's output"),
        run("idlemax job cancel <id>", "stop a job"),
    ];
    if let Some(total) = total.filter(|t| *t > shown as u64) {
        let status = filter.map_or_else(String::new, |s| format!(" --status {s}"));
        let limit = u32::try_from(total).unwrap_or(u32::MAX).min(PAGE_LIMIT_MAX);
        // Asking for more than the coordinator returns would repeat the same page forever.
        let to = if u64::from(limit) < total {
            match filter {
                None => format!("list the newest {limit} of {total}, then narrow it with --status"),
                Some(_) => format!(
                    "list the newest {limit} of {total}; the coordinator answers at most {PAGE_LIMIT_MAX}"
                ),
            }
        } else {
            format!("list all {total}")
        };
        hints.push(run(&format!("idlemax jobs{status} --limit {limit}"), &to));
    }
    hints
}

/// A job that is running or over has output to read; one that waits has its fixes inline.
pub fn why(e: &idlemax_core::explain::Explanation) -> Vec<String> {
    use idlemax_core::explain::ExplainState::{Active, Finished};
    match e.state {
        Active | Finished => vec![run(&format!("idlemax logs {}", e.job), "read its output")],
        _ => Vec::new(),
    }
}

pub fn job_added(id: JobId) -> Vec<String> {
    vec![
        run(&format!("idlemax why {id}"), "see when it will start"),
        run(&format!("idlemax logs {id}"), "read its output"),
        run(&format!("idlemax job cancel {id}"), "stop it"),
    ]
}

pub fn job_changed() -> Vec<String> {
    vec![run("idlemax jobs", "list the remaining jobs")]
}

pub fn control(req: &ControlRequest) -> Vec<String> {
    if req.action == ControlAction::Resume {
        return vec![run("idlemax jobs", "see what is running")];
    }
    vec![run(&resume_command(&req.scope), "let jobs run again")]
}

/// `idlemax resume` for the scope that was just held: a bare resume leaves a held
/// device, account or job held.
fn resume_command(scope: &ControlScope) -> String {
    match scope {
        ControlScope::Global => "idlemax resume".into(),
        ControlScope::Device(d) => format!("idlemax resume --device {d}"),
        ControlScope::Account(a) => format!("idlemax resume --account {a}"),
        ControlScope::Job(j) => format!("idlemax resume --job {j}"),
    }
}

pub fn advice(open: usize) -> Vec<String> {
    if open == 0 {
        return vec![run("idlemax advise refresh", "run the advisor now")];
    }
    vec![
        run("idlemax advise accept <id>", "queue the suggested job"),
        run("idlemax advise dismiss <id>", "drop a suggestion"),
        run(
            "idlemax advise list --fields why,confidence",
            "read the reasoning",
        ),
    ]
}

pub fn quota(accounts: usize) -> Vec<String> {
    if accounts == 0 {
        return vec![run(
            "idlemax init",
            "create config.toml, then add [[quota.accounts]]",
        )];
    }
    vec![run("idlemax advise", "see how to spend spare quota")]
}

pub fn events(shown: usize, limit: u32) -> Vec<String> {
    if shown as u32 >= limit {
        let next = limit.saturating_mul(4).min(PAGE_LIMIT_MAX);
        if next > limit {
            return vec![run(
                &format!("idlemax events --limit {next}"),
                "see older events",
            )];
        }
        // At the cap there is no wider page; say so instead of repeating the request.
        return vec![run(
            &format!("idlemax events --limit {PAGE_LIMIT_MAX}"),
            &format!("see the newest {PAGE_LIMIT_MAX} only: the coordinator answers at most {PAGE_LIMIT_MAX}"),
        )];
    }
    Vec::new()
}

pub fn schedules(shown: usize) -> Vec<String> {
    if shown == 0 {
        return vec![run(
            "idlemax schedule add <spec.toml> --every 1d",
            "repeat a job",
        )];
    }
    vec![
        run("idlemax schedule pause <id>", "stop one queueing"),
        run("idlemax schedule rm <id>", "remove one"),
    ]
}

pub fn schedule_added() -> Vec<String> {
    vec![run("idlemax schedule list", "see what each is waiting for")]
}

pub fn inbox(shown: usize) -> Vec<String> {
    if shown == 0 {
        return vec![run("idlemax jobs --status succeeded", "find finished jobs")];
    }
    vec![run("idlemax inbox show <job-id>", "read a result note")]
}

pub fn init() -> Vec<String> {
    vec![
        run("idlemax coord", "start the coordinator (one machine)"),
        run("idlemax agent", "start the device agent"),
        run(
            "idlemax service install",
            "keep the agent running in the background",
        ),
    ]
}

/// One follow-up command: "Run `cmd` to <to>".
pub fn then(cmd: &str, to: &str) -> Vec<String> {
    vec![run(cmd, to)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_hold_is_resumed_on_the_same_scope() {
        let req = |action, scope| ControlRequest {
            action,
            scope,
            reason: None,
        };
        // Resuming the same scope is the only command that lets those jobs run again.
        assert_eq!(
            control(&req(
                ControlAction::Pause,
                ControlScope::Device("mac".into())
            )),
            ["Run `idlemax resume --device mac` to let jobs run again"]
        );
        assert_eq!(
            control(&req(
                ControlAction::StopAll,
                ControlScope::Account("claude".into())
            )),
            ["Run `idlemax resume --account claude` to let jobs run again"]
        );
        assert_eq!(
            control(&req(ControlAction::StopAll, ControlScope::Job(7))),
            ["Run `idlemax resume --job 7` to let jobs run again"]
        );
        assert_eq!(
            control(&req(ControlAction::Pause, ControlScope::Global)),
            ["Run `idlemax resume` to let jobs run again"]
        );
        assert_eq!(
            control(&req(
                ControlAction::Resume,
                ControlScope::Device("mac".into())
            )),
            ["Run `idlemax jobs` to see what is running"]
        );
    }

    #[test]
    fn a_truncated_job_list_says_how_to_see_all_of_it_with_the_same_filter() {
        let hints = jobs(20, Some(143), Some(JobStatus::Failed));
        assert!(hints.contains(
            &"Run `idlemax jobs --status failed --limit 143` to list all 143".to_string()
        ));
        assert_eq!(jobs(20, Some(20), None).len(), 3);
        assert_eq!(
            jobs(0, Some(0), None),
            ["Run `idlemax job add <spec.toml>` to queue a job"]
        );
    }

    #[test]
    fn a_hint_never_asks_for_more_than_the_coordinator_returns() {
        // Past 1000 the server cuts the page, so `--limit 1500` would answer the same
        // 1000 rows and the same hint forever.
        assert!(jobs(1000, Some(1500), None).contains(&format!(
            "Run `idlemax jobs --limit {PAGE_LIMIT_MAX}` to list the newest {PAGE_LIMIT_MAX} of 1500, then narrow it with --status"
        )));
        assert!(jobs(1000, Some(1200), Some(JobStatus::Failed)).contains(&format!(
            "Run `idlemax jobs --status failed --limit {PAGE_LIMIT_MAX}` to list the newest {PAGE_LIMIT_MAX} of 1200; the coordinator answers at most {PAGE_LIMIT_MAX}"
        )));
        assert_eq!(
            events(50, 50),
            ["Run `idlemax events --limit 200` to see older events"]
        );
        assert_eq!(
            events(800, 800),
            [format!(
                "Run `idlemax events --limit {PAGE_LIMIT_MAX}` to see older events"
            )]
        );
        assert_eq!(
            events(1000, 1000),
            [format!(
                "Run `idlemax events --limit {PAGE_LIMIT_MAX}` to see the newest {PAGE_LIMIT_MAX} only: the coordinator answers at most {PAGE_LIMIT_MAX}"
            )]
        );
        assert!(events(49, 50).is_empty());
    }
}
