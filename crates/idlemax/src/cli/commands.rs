//! Delivery: the subcommand definitions of the command line. Arguments and help text only;
//! `dispatch` decides what each one runs.

use clap::{Args, Subcommand};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, JobId, JobStatus};

use super::{
    config_cmd, device, hooks, init_cmd, job, mode, plan, project, report, review, schedule,
    service, setup, skill, statusline, workers,
};

#[derive(Subcommand, Debug)]
pub(super) enum Cmd {
    /// Run the device agent (one per device)
    Agent,
    /// Run the coordinator
    Coord,
    /// Create config.toml and the API token in $IDLEMAX_HOME (never overwrites)
    Init(init_cmd::InitArgs),
    /// Cluster summary and devices
    Status,
    /// List jobs
    Jobs {
        /// queued, leased, running, paused, succeeded, failed, killed, cancelled, parked
        #[arg(long)]
        status: Option<JobStatus>,
        /// Rows to ask for; the coordinator answers at most 1000
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Add or cancel a job
    #[command(subcommand)]
    Job(JobCmd),
    /// Repeat a job on an interval, such as every night
    #[command(subcommand)]
    Schedule(schedule::ScheduleCmd),
    /// Explain why a job is not running, and what to change
    Why { id: JobId },
    /// The projects the dispatcher may work in; every one is off until you switch it on
    #[command(subcommand)]
    Project(project::ProjectCmd),
    /// Switch the dispatcher off, to propose, where it only shows what it would launch, or to
    /// auto, where it launches the planned workers
    Mode(mode::ModeArgs),
    /// The devices the dispatcher may use; every one is off until you switch it on
    #[command(subcommand)]
    Device(device::DeviceCmd),
    /// See which workers the dispatcher would launch now and why
    Plan(plan::PlanArgs),
    /// Stop every AI worker with its state saved; nothing launches until `idlemax start`
    Stop(workers::StopArgs),
    /// Let the workers `idlemax stop` stopped continue their sessions
    Start,
    /// See the finished tasks per project, and keep or discard each one
    Review(review::ReviewArgs),
    /// Pause jobs everywhere, or on one device, account or job
    Pause(Target),
    /// Resume paused jobs
    Resume(Target),
    /// Kill running jobs and hold new ones (kill switch)
    StopAll(Target),
    /// AI account windows, reserve and headroom
    Quota,
    /// What idlemax did with the machine, and what stopped it
    Report(report::ReportArgs),
    /// Recent events
    Events {
        /// Rows to ask for; the coordinator answers at most 1000
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Print the job log from this device
    Logs {
        /// Job ID
        id: JobId,
        /// Number of lines to show from the end of the log
        #[arg(long)]
        tail: Option<usize>,
        /// Follow new log output
        #[arg(short, long)]
        follow: bool,
    },
    /// List or show job result notes from the inbox (default: list)
    Inbox {
        #[command(subcommand)]
        cmd: Option<InboxCmd>,
    },
    /// AI advisor recommendations (default: list)
    Advise {
        #[command(subcommand)]
        cmd: Option<AdviseCmd>,
    },
    /// Show or change settings in config.toml and apply them to the running agent
    Config(config_cmd::ConfigArgs),
    /// Set this device up in one command: init, the background service, then doctor (safe to repeat)
    Setup(setup::SetupArgs),
    /// Install or remove the background service (LaunchAgent or systemd user unit)
    Service(service::ServiceArgs),
    /// Wire the Claude Code statusline into the quota probe
    Statusline(statusline::StatuslineArgs),
    /// Interactive dashboard
    Tui,
    /// MCP server on stdio for agent CLIs: status, start, stop, project on/off, finished work
    Mcp,
    /// Check this device's setup; prints one line per check and the fix for anything wrong
    Doctor,
    /// Talk to this device's agent directly; works while the coordinator is down
    #[command(subcommand)]
    Local(LocalCmd),
    /// Session hooks for coding agents
    Hooks(hooks::HooksArgs),
    /// Print the agent skill, or check the committed copy is current
    Skill(skill::SkillArgs),
}

#[derive(Subcommand, Debug)]
pub(super) enum JobCmd {
    /// Queue a job from a spec file, stdin (`-`) or a command after `--`
    Add(Box<job::AddArgs>),
    /// Cancel a job (kills it if running)
    Cancel { id: JobId },
}

#[derive(Subcommand, Debug)]
pub enum InboxCmd {
    /// List notes in the inbox (default)
    List,
    /// Print the note markdown for a job or a notice
    Show {
        /// Job ID, or n<id> for a notice
        #[arg(value_parser = super::inbox::parse_note_ref)]
        id: super::inbox::NoteRef,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdviseCmd {
    /// List recommendations, open ones first
    List,
    /// Queue the recommendation's suggested job
    Accept { id: i64 },
    /// Close the recommendation without acting
    Dismiss { id: i64 },
    /// Re-run the advisor now
    Refresh,
}

#[derive(Subcommand, Debug)]
pub enum LocalCmd {
    /// Agent state on this device
    Status,
    /// Pause every job on this device
    Pause,
    /// Resume after a local pause
    Resume,
    /// Kill every job on this device and hold new ones
    StopAll,
}

/// Control target; no flag means the whole cluster.
#[derive(Args, Debug)]
pub(super) struct Target {
    #[arg(long, conflicts_with_all = ["account", "job"])]
    device: Option<String>,
    #[arg(long, conflicts_with = "job")]
    account: Option<String>,
    #[arg(long, value_name = "ID")]
    job: Option<JobId>,
    /// Recorded in the event log
    #[arg(long)]
    reason: Option<String>,
}

impl Target {
    pub(super) fn request(self, action: ControlAction) -> ControlRequest {
        let scope = match (self.device, self.account, self.job) {
            (Some(d), _, _) => ControlScope::Device(d),
            (_, Some(a), _) => ControlScope::Account(a),
            (_, _, Some(j)) => ControlScope::Job(j),
            _ => ControlScope::Global,
        };
        ControlRequest {
            scope,
            action,
            reason: self.reason,
        }
    }
}
