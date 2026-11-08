//! Delivery: next-step hints for TOON replies (AXI contextual disclosure). A few complete
//! commands that follow from what was just shown; `<id>` stands for a value the reader picks.

use idlemax_core::types::{JobId, JobStatus};

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
        hints.push(run(
            &format!("idlemax jobs{status} --limit {total}"),
            &format!("list all {total}"),
        ));
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

pub fn control(resumed: bool) -> Vec<String> {
    if resumed {
        vec![run("idlemax jobs", "see what is running")]
    } else {
        vec![run("idlemax resume", "let jobs run again")]
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
        return vec![run(
            &format!("idlemax events --limit {}", limit * 4),
            "see older events",
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
}
