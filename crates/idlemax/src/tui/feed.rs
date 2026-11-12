//! Delivery: background task that polls the coordinator and performs dashboard actions.

use std::future::Future;
use std::sync::mpsc::Sender;
use std::time::Duration;

use idlemax_core::config::{LimitRequest, Setting};
use idlemax_core::dispatch::label::plain;
use idlemax_core::dispatch::view::DispatcherView;
use idlemax_core::explain::{Explanation, WhySummary};
use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::quota::QuotaView;
use idlemax_core::report::Report;
use idlemax_core::schedule::{ScheduleId, ScheduleView};
use idlemax_core::types::{Event, Job, JobId, Recommendation, UnixMs};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::MissedTickBehavior;

use crate::cli::rows::Zone;
use crate::cli::{print, report, ApiClient, ClientError, Proposal};
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
    /// None from a coordinator without `/v1/dispatcher`; the Dispatch tab then says so.
    pub dispatcher: Option<DispatcherView>,
    /// The owner limits (empty from a coordinator without `/v1/limits`).
    pub limits: Vec<Setting>,
    /// The rule proposals (empty from a coordinator without `/v1/proposals`).
    pub proposals: Vec<Proposal>,
    /// One line per optional feed that failed for another reason than a missing route.
    pub notes: Vec<String>,
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
    StopWorkers,
    StartWorkers,
    /// The mode by name: `auto` reaches the coordinator even before this build knows it.
    SetMode(String),
    SwitchProject {
        id: i64,
        name: String,
        on: bool,
    },
    /// Switch a device on or off for dispatcher work.
    DeviceSwitch {
        id: String,
        on: bool,
    },
    /// `value` None puts the limit back to its default.
    SetLimit {
        key: String,
        value: Option<String>,
    },
    ApproveRule(i64),
    RejectRule(i64),
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
            Request::StopWorkers => "stop workers".into(),
            Request::StartWorkers => "start workers".into(),
            Request::SetMode(mode) => format!("mode {mode}"),
            Request::SwitchProject { name, on, .. } => {
                format!("project {name} {}", if *on { "on" } else { "off" })
            }
            Request::DeviceSwitch { id, on } => {
                format!("device {id} {}", if *on { "on" } else { "off" })
            }
            Request::SetLimit { key, value: None } => format!("{key} back to its default"),
            Request::SetLimit {
                key,
                value: Some(v),
            } => format!("{key} = {v}"),
            Request::ApproveRule(id) => format!("rule {id} added to rules.md"),
            Request::RejectRule(id) => format!("rule {id} rejected"),
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
    let (all, (dispatcher, limits, proposals)) = tokio::join!(
        async {
            tokio::try_join!(
                c.status(),
                c.jobs(None, JOB_LIMIT),
                c.quota(),
                c.advice(),
                c.events(EVENT_LIMIT),
                or_empty(c.schedules()),
                or_empty(c.why_summaries(JOB_LIMIT)),
            )
        },
        async { tokio::join!(c.dispatcher(), c.limits(), c.proposals()) }
    );
    // The dispatcher's feeds are best effort: a failing one (an invalid config.toml, a
    // failing plan) must not blank every tab when the owner most needs to see what is wrong.
    let mut notes = Vec::new();
    let dispatcher = optional(dispatcher, "dispatcher view", &mut notes);
    let limits = optional(limits, "limits", &mut notes).unwrap_or_default();
    let mut proposals = optional(proposals, "rule proposals", &mut notes).unwrap_or_default();
    for p in &mut proposals {
        // Text the orchestrator wrote goes to a terminal: one line, no escape.
        p.line = plain(&p.line);
        p.reason = plain(&p.reason);
    }
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
                dispatcher,
                limits,
                proposals,
                notes,
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

/// One optional feed: None from a coordinator that predates it (404), and None with a note
/// for any other failure.
fn optional<T>(r: Result<T, ClientError>, what: &str, notes: &mut Vec<String>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(ClientError::Api { status: 404, .. }) => None,
        Err(e) => {
            notes.push(format!("{what} not available: {}", e.describe()));
            None
        }
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
        Request::StopWorkers => c.stop_all_workers("tui").await.map(|_| ()),
        Request::StartWorkers => c.start_workers().await.map(|_| ()),
        Request::SetMode(mode) => c.set_mode_named(&mode).await.map(|_| ()),
        Request::SwitchProject { id, on, .. } => c.switch_project(id, on).await.map(|_| ()),
        Request::DeviceSwitch { id, on } => c.switch_device(&id, on).await.map(|_| ()),
        Request::SetLimit { key, value } => {
            let req = LimitRequest { key, value };
            c.set_limit(&req).await.map(|_| ())
        }
        Request::ApproveRule(id) => c.decide_proposal(id, true).await.map(|_| ()),
        Request::RejectRule(id) => c.decide_proposal(id, false).await.map(|_| ()),
    };
    match result {
        Ok(()) => Update::Done(label),
        Err(e) => Update::Failed(format!("{label}: {}", e.describe())),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn a_failing_optional_feed_is_a_note_and_the_rest_still_shows() {
        let list = || get(|| async { Json(json!([])) });
        let router = Router::new()
            .route("/v1/status", get(|| async { Json(json!({})) }))
            .route("/v1/jobs", list())
            .route("/v1/quota", list())
            .route("/v1/advice", list())
            .route("/v1/events", list())
            .route(
                "/v1/limits",
                get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = ApiClient::new(&url, "t".repeat(32)).unwrap();
        let Update::Snapshot(snap, _) = fetch(&client).await else {
            panic!("the snapshot failed");
        };
        assert!(snap.dispatcher.is_none() && snap.limits.is_empty());
        assert_eq!(snap.notes.len(), 1, "{:?}", snap.notes);
        assert!(snap.notes[0].starts_with("limits not available"));
    }
}
