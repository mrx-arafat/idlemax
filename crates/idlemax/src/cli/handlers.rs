//! Delivery: the coordinator-backed subcommands. Each reads what it needs, skips a mutation
//! that would change nothing, and hands the value, a table renderer, a TOON view and the
//! next-step hints to the output boundary.

use anyhow::Result;
use idlemax_core::dispatch::view::DispatcherView;
use idlemax_core::protocol::{ClusterStatus, ControlRequest};
use idlemax_core::types::{ControlAction, ControlScope, JobId, JobStatus, UnixMs};

use super::out::dispatcher_view::{self, StatusOut};
use super::out::toon::Node;
use super::out::{hints, lists, views};
use super::rows::Zone;
use super::{noop, print, AdviseCmd, ApiClient, ClientError, LocalCmd, Out};
use crate::state::now_ms;

/// The cluster status and, from a coordinator that serves it, the dispatcher view.
pub async fn status(c: &ApiClient, out: &Out, now: UnixMs) -> Result<()> {
    let (s, view) = tokio::join!(c.status(), c.dispatcher());
    let s = s?;
    // Best effort: `status` is what the owner runs to see why things are wrong.
    let view = match view {
        Ok(view) => Some(view),
        Err(ClientError::Api { status: 404, .. }) => None,
        Err(e @ ClientError::Api { status: 401, .. }) => return Err(e.into()),
        Err(e) => {
            eprintln!("note: the dispatcher view is not available: {e}");
            None
        }
    };
    let both = StatusOut {
        status: &s,
        dispatcher: view.as_ref(),
    };
    out.emit_with(
        &both,
        |b| print::status(b.status, b.dispatcher, now),
        || status_node(&s, view.as_ref(), now, out.fields()),
        &hints::status(s.devices.len(), s.global_paused),
    )
}

/// The TOON of `idlemax status`, also the text of the MCP `status` tool.
pub(super) fn status_node(
    s: &ClusterStatus,
    view: Option<&DispatcherView>,
    now: UnixMs,
    fields: &[String],
) -> Result<Node> {
    let mut node = views::status(s, now, fields)?;
    if let (Some(v), Node::Obj(fields)) = (view, &mut node) {
        fields.push(("dispatcher".into(), dispatcher_view::node(v, now)));
    }
    Ok(node)
}

pub async fn why(c: &ApiClient, id: JobId, out: &Out) -> Result<()> {
    let e = c.why(id).await?;
    out.emit_with(
        &e,
        super::why::render,
        || Ok(views::why(&e)),
        &hints::why(&e),
    )
}

pub async fn jobs(
    c: &ApiClient,
    out: &Out,
    now: UnixMs,
    filter: Option<JobStatus>,
    limit: u32,
) -> Result<()> {
    let list = c.jobs(filter, limit).await?;
    let total = if out.is_toon() {
        Some(views::job_total(&c.status().await?.jobs, filter))
    } else {
        None
    };
    out.emit_with(
        &list,
        |j| print::jobs(j, now),
        || Ok(lists::jobs(&list, total, filter, now, out.fields())?),
        &hints::jobs(list.len(), total, filter),
    )
}

pub async fn control(c: &ApiClient, req: ControlRequest, out: &Out) -> Result<()> {
    let before = c.status().await?;
    let finished_job = match (req.action, &req.scope) {
        (ControlAction::StopAll, ControlScope::Job(id)) => noop::cancel(&c.job(*id).await?),
        _ => None,
    };
    let repeated = finished_job
        .map(|status| status.to_string())
        .or_else(|| noop::control(&req, &before).map(str::to_owned));
    let (s, label, result) = match repeated {
        Some(state) => {
            let result = noop::already(state);
            (before, result.clone(), result)
        }
        None => (
            c.control(&req).await?,
            print::control_label(&req),
            views::done(&req),
        ),
    };
    let next = if finished_job.is_some() {
        hints::job_changed()
    } else {
        hints::control(&req)
    };
    out.emit_with(
        &s,
        |s| format!("ok: {label}\n{}", print::summary(s)),
        || Ok(views::control(&result, &s)),
        &next,
    )
}

pub async fn cancel(c: &ApiClient, id: JobId, out: &Out) -> Result<()> {
    let known = c.job(id).await?;
    if let Some(status) = noop::cancel(&known) {
        let result = noop::already(status);
        return out.emit_with(
            &known,
            |j| format!("job {} {result}: {}", j.id, j.spec.name),
            || Ok(views::job_result(&result, &known)),
            &hints::job_changed(),
        );
    }
    let job = c.cancel_job(id).await?;
    out.emit_with(
        &job,
        print::job_line,
        || Ok(views::job_result("cancelled", &job)),
        &hints::job_changed(),
    )
}

pub async fn quota(c: &ApiClient, out: &Out, now: UnixMs) -> Result<()> {
    let (views_in, status) = tokio::try_join!(c.quota(), c.status())?;
    out.emit_with(
        &views_in,
        |v| print::quota(v, &status.paused_accounts, now),
        || {
            let windows = lists::quota(&views_in, &status.paused_accounts, now, out.fields())?;
            Ok(crate::cli::out::budget::with_budgets(
                windows, &views_in, now,
            ))
        },
        &hints::quota(views_in.len()),
    )
}

pub async fn events(c: &ApiClient, out: &Out, limit: u32, zone: Zone) -> Result<()> {
    // Ask for what the coordinator will return, so the count and the hint agree with it.
    let limit = limit.clamp(1, hints::PAGE_LIMIT_MAX);
    let list = c.events(limit).await?;
    out.emit_with(
        &list,
        |e| print::events(e, zone),
        || Ok(lists::events(&list, zone, out.fields())?),
        &hints::events(list.len(), limit),
    )
}

pub async fn advise(c: &ApiClient, cmd: AdviseCmd, out: &Out, now: UnixMs) -> Result<()> {
    match cmd {
        AdviseCmd::List => advice_list(c.advice().await?, out, now),
        AdviseCmd::Refresh => advice_list(c.refresh_advice().await?, out, now),
        AdviseCmd::Accept { id } => {
            let job = c.accept_advice(id).await?;
            let result = format!("accepted advice {id}, job queued");
            out.emit_with(
                &job,
                print::job_line,
                || Ok(views::job_result(&result, &job)),
                &hints::job_added(job.id),
            )
        }
        AdviseCmd::Dismiss { id } => dismiss(c, id, out).await,
    }
}

fn advice_list(
    list: Vec<idlemax_core::types::Recommendation>,
    out: &Out,
    now: UnixMs,
) -> Result<()> {
    let open = list
        .iter()
        .filter(|r| r.status == idlemax_core::types::RecommendationStatus::Open)
        .count();
    out.emit_with(
        &list,
        |a| print::advice(a, now),
        || Ok(lists::advice(&list, now, out.fields())?),
        &hints::advice(open),
    )
}

async fn dismiss(c: &ApiClient, id: i64, out: &Out) -> Result<()> {
    let known = c.advice().await?.into_iter().find(|r| r.id == id);
    if let Some(rec) = known.filter(noop::dismiss) {
        let result = noop::already("dismissed");
        return out.emit_with(
            &rec,
            |r| format!("advice {} {result}: {}", r.id, r.title),
            || Ok(views::advice_result(&result, &rec)),
            &hints::advice(0),
        );
    }
    let rec = c.dismiss_advice(id).await?;
    out.emit_with(
        &rec,
        print::advice_line,
        || Ok(views::advice_result("dismissed", &rec)),
        &hints::advice(0),
    )
}

/// `idlemax local ...`: this device's agent, which works while the coordinator is down.
pub async fn local(cmd: LocalCmd, c: &ApiClient, out: &Out) -> Result<()> {
    let action = match cmd {
        LocalCmd::Status => None,
        LocalCmd::Pause => Some(ControlAction::Pause),
        LocalCmd::Resume => Some(ControlAction::Resume),
        LocalCmd::StopAll => Some(ControlAction::StopAll),
    };
    let before = c.local_status().await?;
    let (s, result) = match action {
        None => (before, None),
        Some(action) => match noop::local(action, &before) {
            Some(noop::LocalNoop::State(state)) => (before, Some(noop::already(state))),
            Some(noop::LocalNoop::KillSwitch) => (
                before,
                Some(format!(
                    "already paused by the kill switch (no-op); {}",
                    noop::KILL_SWITCH_HOLDS
                )),
            ),
            None => {
                let req = ControlRequest {
                    scope: ControlScope::Global,
                    action,
                    reason: None,
                };
                let mut done = format!("{} this device", views::verb(action));
                let s = c.local_control(&req).await?;
                if s.kill_switch {
                    done.push_str(&format!("; {}", noop::KILL_SWITCH_HOLDS));
                }
                (s, Some(done))
            }
        },
    };
    let now = now_ms();
    out.emit_with(
        &s,
        |s| print::local_status(result.as_deref(), s, now),
        || Ok(views::local(result.as_deref(), &s, now)),
        &[],
    )
}
