//! Delivery: background task that polls the coordinator and performs dashboard actions.

use std::sync::mpsc::Sender;
use std::time::Duration;

use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{Event, Job, JobId, Recommendation, UnixMs};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::MissedTickBehavior;

use crate::cli::{print, ApiClient};
use crate::state::now_ms;

const REFRESH: Duration = Duration::from_secs(2);
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
}

/// An action the user confirmed in the dashboard.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Control(ControlRequest),
    CancelJob(JobId),
    AcceptAdvice(i64),
    DismissAdvice(i64),
    RefreshAdvice,
}

impl Request {
    pub fn label(&self) -> String {
        match self {
            Request::Control(r) => print::control_label(r),
            Request::CancelJob(id) => format!("cancel job {id}"),
            Request::AcceptAdvice(id) => format!("accept advice {id}"),
            Request::DismissAdvice(id) => format!("dismiss advice {id}"),
            Request::RefreshAdvice => "refresh advice".into(),
        }
    }
}

#[derive(Debug)]
pub enum Update {
    Snapshot(Box<Snapshot>, UnixMs),
    FetchFailed(String),
    Done(String),
    Failed(String),
}

/// Runs until the dashboard drops either channel end.
pub async fn run(
    client: ApiClient,
    mut requests: UnboundedReceiver<Request>,
    updates: Sender<Update>,
) {
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            req = requests.recv() => {
                let Some(req) = req else { return };
                if updates.send(perform(&client, req).await).is_err() {
                    return;
                }
            }
        }
        if updates.send(fetch(&client).await).is_err() {
            return;
        }
    }
}

async fn fetch(c: &ApiClient) -> Update {
    let all = tokio::try_join!(
        c.status(),
        c.jobs(None, JOB_LIMIT),
        c.quota(),
        c.advice(),
        c.events(EVENT_LIMIT)
    );
    match all {
        Ok((status, jobs, quota, advice, events)) => Update::Snapshot(
            Box::new(Snapshot {
                status,
                jobs,
                quota,
                advice,
                events,
            }),
            now_ms(),
        ),
        Err(e) => Update::FetchFailed(e.describe()),
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
    };
    match result {
        Ok(()) => Update::Done(label),
        Err(e) => Update::Failed(format!("{label}: {}", e.describe())),
    }
}
