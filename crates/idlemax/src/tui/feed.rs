//! Delivery: background task that polls the coordinator and performs dashboard actions.

use std::future::Future;
use std::sync::mpsc::Sender;
use std::time::Duration;

use idlemax_core::explain::{Explanation, WhySummary};
use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::quota::QuotaView;
use idlemax_core::report::Report;
use idlemax_core::schedule::{ScheduleId, ScheduleView};
use idlemax_core::types::{Event, Job, JobId, Recommendation, UnixMs};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::MissedTickBehavior;

use crate::cli::rows::Zone;
use crate::cli::{print, report, ApiClient, ClientError};
use crate::state::now_ms;

const REFRESH: Duration = Duration::from_secs(2);
/// The report changes slowly and costs the coordinator a scan, so it is asked for less often.
const REPORT_REFRESH: Duration = Duration::from_secs(30);
const JOB_LIMIT: u32 = 200;
const EVENT_LIMIT: u32 = 200;

/// Everything the dashboard shows, fetched together.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub status: ClusterStatus,
    pub jobs: Vec<Job>,
    pub quota: Vec<QuotaView>,
    pub advice: Vec<Recommendation>,
    pub events: Vec<Event>,
    pub schedules: Vec<ScheduleView>,
    /// A headline reason for each waiting job (empty from a coordinator without `/v1/why`).
    pub why: Vec<WhySummary>,
}

/// An action the user confirmed in the dashboard.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Control(ControlRequest),
    CancelJob(JobId),
    AcceptAdvice(i64),
    DismissAdvice(i64),
    RefreshAdvice,
    PauseSchedule(ScheduleId),
    ResumeSchedule(ScheduleId),
    RemoveSchedule(ScheduleId),
    /// The full explanation of one job, for the detail pane.
    Why(JobId),
}

impl Request {
    pub fn label(&self) -> String {
        match self {
            Request::Control(r) => print::control_label(r),
            Request::CancelJob(id) => format!("cancel job {id}"),
            Request::AcceptAdvice(id) => format!("accept advice {id}"),
            Request::DismissAdvice(id) => format!("dismiss advice {id}"),
            Request::RefreshAdvice => "refresh advice".into(),
            Request::PauseSchedule(id) => format!("pause schedule {id}"),
            Request::ResumeSchedule(id) => format!("resume schedule {id}"),
            Request::RemoveSchedule(id) => format!("remove schedule {id}"),
            Request::Why(id) => format!("explain job {id}"),
        }
    }
}

#[derive(Debug)]
pub enum Update {
    Snapshot(Box<Snapshot>, UnixMs),
    FetchFailed(String),
    Done(String),
    Failed(String),
    /// The answer to `Request::Why`, or why it could not be had.
    Why(JobId, Result<Explanation, String>),
    /// Today's report and the last 7 days'.
    Report(Vec<Report>),
}

/// Runs until the dashboard drops either channel end.
pub async fn run(
    client: ApiClient,
    zone: Zone,
    mut requests: UnboundedReceiver<Request>,
    updates: Sender<Update>,
) {
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut report_tick = tokio::time::interval(REPORT_REFRESH);
    report_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = report_tick.tick() => {
                if let Some(update) = fetch_report(&client, zone).await {
                    if updates.send(update).is_err() {
                        return;
                    }
                }
                continue;
            }
            req = requests.recv() => {
                let Some(req) = req else { return };
                // An explanation changes nothing, so it needs no refresh of everything else.
                let only_reads = matches!(req, Request::Why(_));
                if updates.send(perform(&client, req).await).is_err() {
                    return;
                }
                if only_reads {
                    continue;
                }
            }
        }
        if updates.send(fetch(&client).await).is_err() {
            return;
        }
    }
}

/// The reports, an empty list from a coordinator that predates them, or None when the
/// coordinator could not be asked (the last answer stays on screen).
async fn fetch_report(c: &ApiClient, zone: Zone) -> Option<Update> {
    let [today, week] = report::default_periods(now_ms(), zone);
    match tokio::try_join!(c.report(today), c.report(week)) {
        Ok((today, week)) => Some(Update::Report(vec![today, week])),
        Err(ClientError::Api { status: 404, .. }) => Some(Update::Report(Vec::new())),
        Err(_) => None,
    }
}

async fn fetch(c: &ApiClient) -> Update {
    let all = tokio::try_join!(
        c.status(),
        c.jobs(None, JOB_LIMIT),
        c.quota(),
        c.advice(),
        c.events(EVENT_LIMIT),
        or_empty(c.schedules()),
        or_empty(c.why_summaries(JOB_LIMIT))
    );
    match all {
        Ok((status, jobs, quota, advice, events, schedules, why)) => Update::Snapshot(
            Box::new(Snapshot {
                status,
                jobs,
                quota,
                advice,
                events,
                schedules,
                why,
            }),
            now_ms(),
        ),
        Err(e) => Update::FetchFailed(e.describe()),
    }
}

/// A coordinator older than schedules or `/v1/why` answers 404; that is an empty list,
/// not an outage.
async fn or_empty<T>(
    call: impl Future<Output = Result<Vec<T>, ClientError>>,
) -> Result<Vec<T>, ClientError> {
    match call.await {
        Err(ClientError::Api { status: 404, .. }) => Ok(Vec::new()),
        other => other,
    }
}

async fn perform(c: &ApiClient, req: Request) -> Update {
    let label = req.label();
    let result = match req {
        Request::Control(r) => c.control(&r).await.map(|_| ()),
        Request::CancelJob(id) => c.cancel_job(id).await.map(|_| ()),
        Request::AcceptAdvice(id) => c.accept_advice(id).await.map(|_| ()),
        Request::DismissAdvice(id) => c.dismiss_advice(id).await.map(|_| ()),
        Request::RefreshAdvice => c.refresh_advice().await.map(|_| ()),
        Request::PauseSchedule(id) => c.pause_schedule(id).await.map(|_| ()),
        Request::ResumeSchedule(id) => c.resume_schedule(id).await.map(|_| ()),
        Request::RemoveSchedule(id) => c.remove_schedule(id).await.map(|_| ()),
        Request::Why(id) => return Update::Why(id, c.why(id).await.map_err(|e| e.describe())),
    };
    match result {
        Ok(()) => Update::Done(label),
        Err(e) => Update::Failed(format!("{label}: {}", e.describe())),
    }
}
