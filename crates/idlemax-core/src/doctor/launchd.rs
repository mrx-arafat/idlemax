//! Domain: what launchd says about an idlemax LaunchAgent, read from `launchctl print` text.
//! Pure: the adapter runs launchctl; this module only reads the text it returns.

use super::Check;

/// What `launchctl print gui/<uid>/<label>` told the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Print {
    /// Exit 0: launchd has the job loaded, and this is its dump.
    Dump(String),
    /// Non-zero exit: launchd has no such job in this session.
    NotLoaded,
    /// launchctl could not be asked; why, for the detail.
    Unavailable(&'static str),
}

/// The keys launchd prints at the top level of a job's dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Job {
    pub state: Option<String>,
    pub runs: Option<u32>,
    pub last_exit_code: Option<String>,
    pub last_exit_reason: Option<String>,
}

impl Job {
    /// Why the previous start ended badly: launchd's reason, else a non-zero exit code.
    fn previous_failure(&self) -> Option<String> {
        if let Some(reason) = &self.last_exit_reason {
            return Some(reason.clone());
        }
        match self.last_exit_code.as_deref()?.parse::<i32>() {
            Ok(code) if code != 0 => Some(format!("exit code {code}")),
            _ => None,
        }
    }
}

/// Reads the top-level keys of a `launchctl print` dump. Nested blocks (endpoints, environment)
/// hold keys of the same name, so only lines one tab deep count.
pub fn parse(dump: &str) -> Job {
    let mut job = Job::default();
    for line in dump.lines() {
        let Some(field) = line
            .strip_prefix('\t')
            .filter(|field| !field.starts_with(char::is_whitespace))
        else {
            continue;
        };
        let Some((key, value)) = field.split_once(" = ") else {
            continue;
        };
        let value = value.trim();
        match key {
            "state" => job.state = Some(value.to_string()),
            "runs" => job.runs = value.parse().ok(),
            "last exit code" => job.last_exit_code = Some(value.to_string()),
            "last exit reason" => job.last_exit_reason = Some(value.to_string()),
            _ => {}
        }
    }
    job
}

/// True when the plist sets `<key>Label</key>` to `label`. The adapter asks launchd only about a
/// job that the plist file itself declares.
pub fn declares_label(plist: &str, label: &str) -> bool {
    plist
        .split_once("<key>Label</key>")
        .and_then(|(_, rest)| rest.trim_start().strip_prefix("<string>"))
        .and_then(|rest| rest.strip_prefix(label))
        .is_some_and(|rest| rest.starts_with("</string>"))
}

/// The doctor check for one idlemax LaunchAgent. `kind` is "agent" or "coordinator".
pub fn verdict(kind: &str, label: &str, print: Print) -> Check {
    let id = format!("launchd-{kind}");
    match print {
        Print::Unavailable(why) => Check::warn(
            id,
            format!("launchd was not asked about {kind}: {why}"),
            format!("run `launchctl print gui/$(id -u)/{label}` by hand to see the job"),
        ),
        Print::NotLoaded => Check::fail(
            id,
            format!("{kind} is not loaded in launchd"),
            "run `idlemax service install` to load it",
        ),
        Print::Dump(dump) => {
            let job = parse(&dump);
            let failure = job.previous_failure();
            if job.state.as_deref() != Some("running") {
                let detail = match failure {
                    Some(failure) => {
                        format!("{kind} is not running; its last start ended with {failure}")
                    }
                    None => format!(
                        "{kind} is not running (launchd state: {})",
                        job.state.as_deref().unwrap_or("unknown")
                    ),
                };
                return Check::fail(
                    id,
                    detail,
                    "if launchd is retrying a failed start, wait for it and run `idlemax doctor` again; otherwise run `idlemax service install` to reload it",
                );
            }
            match failure {
                Some(failure) => Check::ok(
                    id,
                    format!("{kind} running; its previous start ended with {failure}, and launchd has started it again since"),
                ),
                None => Check::ok(id, format!("{kind} running")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Level;

    const LABEL: &str = "com.idlemax.agent";

    /// `launchctl print` output for a LaunchAgent whose first start was killed by the kernel
    /// and which has started again since. Top-level keys sit one tab deep; nested blocks deeper.
    const RESTARTED: &str = "gui/501/com.idlemax.agent = {\n\
        \tactive count = 1\n\
        \tpath = /Users/x/Library/LaunchAgents/com.idlemax.agent.plist\n\
        \tstate = running\n\
        \tprogram = /Users/x/.cargo/bin/idlemax\n\
        \tendpoints = {\n\
        \t\t\"com.idlemax.agent\" = {\n\
        \t\t\tstate = active\n\
        \t\t}\n\
        \t}\n\
        \truns = 3\n\
        \tpid = 56727\n\
        \tlast exit reason = OS_REASON_CODESIGNING\n\
        }\n";

    #[test]
    fn parse_reads_the_top_level_keys_and_skips_nested_blocks() {
        let job = parse(RESTARTED);
        assert_eq!(job.state.as_deref(), Some("running"));
        assert_eq!(job.runs, Some(3));
        assert_eq!(
            job.last_exit_reason.as_deref(),
            Some("OS_REASON_CODESIGNING")
        );
        assert_eq!(job.last_exit_code, None);
    }

    #[test]
    fn a_running_job_is_ok_and_notes_a_failed_previous_start() {
        let noted = verdict("agent", LABEL, Print::Dump(RESTARTED.into()));
        assert_eq!(noted.level, Level::Ok);
        assert!(
            noted.detail.contains("OS_REASON_CODESIGNING"),
            "{}",
            noted.detail
        );
        assert_eq!(noted.fix, None);

        let clean = RESTARTED.replace("\tlast exit reason = OS_REASON_CODESIGNING\n", "");
        let clean = verdict("agent", LABEL, Print::Dump(clean));
        assert_eq!(clean.level, Level::Ok);
        assert_eq!(clean.detail, "agent running");
    }

    #[test]
    fn a_job_that_is_not_running_fails_and_names_the_last_exit_reason() {
        let stopped = RESTARTED.replace("\tstate = running\n", "\tstate = not running\n");
        let check = verdict("agent", LABEL, Print::Dump(stopped));
        assert_eq!(check.level, Level::Fail);
        assert!(
            check.detail.contains("OS_REASON_CODESIGNING"),
            "{}",
            check.detail
        );
        assert!(check.fix.is_some());
    }

    #[test]
    fn a_job_launchd_does_not_know_fails_and_an_unanswered_launchctl_only_warns() {
        assert_eq!(verdict("agent", LABEL, Print::NotLoaded).level, Level::Fail);
        let unanswered = Print::Unavailable("launchctl did not answer");
        assert_eq!(verdict("agent", LABEL, unanswered).level, Level::Warn);
    }

    #[test]
    fn a_plist_must_declare_the_label_it_is_asked_about() {
        let plist = "<dict>\n\t<key>Label</key>\n\t<string>com.idlemax.agent</string>\n</dict>";
        assert!(declares_label(plist, "com.idlemax.agent"));
        assert!(!declares_label(plist, "com.idlemax.coord"));
        assert!(!declares_label(
            "[Unit]\nDescription=idlemax agent\n",
            "com.idlemax.agent"
        ));
    }
}
