//! Delivery: draws the Dispatch tab from the `DispatcherView`: one pane at a time, as a
//! table under a title with the mode and the line state, and the selected row's details.

use idlemax_core::dispatch::view::{
    DeviceLine, DispatcherView, NeedsYouLine, ProjectLine, WorkerLine,
};
use idlemax_core::types::UnixMs;
use idlemax_core::worker::run::RunState;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, TableState, Wrap};
use ratatui::Frame;
use serde_json::Value;

use super::dispatcher::{open_proposals, LimitInput, Pane};
use super::feed::Snapshot;
use super::table::{self, empty_state, table_in, Spec};
use crate::cli::rows::{rel, snake};
use crate::cli::Proposal;

type Rows = Vec<(Vec<String>, Style)>;

const NO_VIEW: &str = "No dispatcher view: this coordinator predates it, so update it.";
const UNJUDGED: &str = "not judged by the orchestrator";
const DAY_MS: UnixMs = 86_400_000;

pub fn render(
    f: &mut Frame,
    area: Rect,
    d: &Snapshot,
    pane: Pane,
    state: &mut TableState,
    now_ms: UnixMs,
) {
    let Some(v) = &d.dispatcher else {
        // A view that failed says why; one the coordinator lacks says to update it.
        let text = d.notes.first().map_or(NO_VIEW, String::as_str);
        return empty_state(f, area, text);
    };
    let (spec, rows) = pane_table(d, v, pane, now_ms);
    let wanted = u16::try_from(rows.len() + 3).unwrap_or(u16::MAX);
    let [list, detail] = Layout::vertical([
        Constraint::Length(wanted.max(4).min(area.height * 55 / 100)),
        Constraint::Min(4),
    ])
    .areas(area);
    let block = Block::bordered().title(title(v, pane));
    table_in(f, list, spec, rows, state, block);
    let mut lines: Vec<Line> = d
        .notes
        .iter()
        .map(|n| Line::from(n.clone()).red())
        .collect();
    lines.extend(details(d, v, pane, state.selected()));
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(" details "));
    f.render_widget(p, detail);
}

/// " 1/8 workers | mode off | mac hold "
fn title(v: &DispatcherView, pane: Pane) -> String {
    let lines: Vec<String> = v
        .devices
        .iter()
        .map(|d| format!("{} {}", d.id, d.state.as_str()))
        .collect();
    let name = match (pane, &v.limits_device) {
        (Pane::Limits, Some(own)) => format!("limits of {own}"),
        _ => pane.title().to_string(),
    };
    format!(
        " {}/{} {} | mode {} | {} ",
        pane.index() + 1,
        Pane::ALL.len(),
        name,
        v.mode.as_str(),
        lines.join(", ")
    )
}

fn pane_table(
    d: &Snapshot,
    v: &DispatcherView,
    pane: Pane,
    now_ms: UnixMs,
) -> (&'static Spec, Rows) {
    match pane {
        Pane::Workers => (&table::D_WORKERS, worker_rows(&v.workers, now_ms)),
        Pane::Review => (&table::D_REVIEW, worker_rows(&v.review, now_ms)),
        Pane::Launch => (&table::D_LAUNCH, launch_rows(v)),
        Pane::Needs => {
            let row =
                |n: &NeedsYouLine| plain(vec![snake(&n.kind), n.subject.clone(), n.reason.clone()]);
            (&table::D_NEEDS, v.needs_you.iter().map(row).collect())
        }
        Pane::Budget => (&table::D_BUDGET, budget_rows(v, now_ms)),
        Pane::Projects => (
            &table::D_PROJECTS,
            v.projects.iter().map(project_row).collect(),
        ),
        Pane::Devices => {
            let on = |e: Option<bool>| match e {
                Some(true) => "on",
                Some(false) => "off",
                None => "-",
            };
            let row = |x: &DeviceLine| {
                plain(vec![
                    on(x.enabled).into(),
                    x.id.clone(),
                    x.state.as_str().into(),
                    x.reason.clone(),
                ])
            };
            (&table::D_DEVICES, v.devices.iter().map(row).collect())
        }
        Pane::Limits => (&table::D_LIMITS, limit_rows(d, v)),
        Pane::Rules => {
            let row = |p: &&Proposal| {
                plain(vec![
                    p.id.to_string(),
                    p.run.to_string(),
                    p.line.clone(),
                    p.reason.clone(),
                ])
            };
            (&table::D_RULES, open_proposals(d).iter().map(row).collect())
        }
    }
}

fn plain(cells: Vec<String>) -> (Vec<String>, Style) {
    (cells, Style::new())
}

/// What the orchestrator planned, then the ready tasks it did not judge (A19).
fn launch_rows(v: &DispatcherView) -> Rows {
    let planned = v.planned.iter().map(|p| {
        plain(vec![
            p.device.clone(),
            p.project.clone(),
            p.title.clone(),
            p.model.clone(),
            String::new(),
        ])
    });
    let unjudged = v.ready_unjudged.iter().map(|t| {
        let cells = vec![
            String::new(),
            t.project.clone(),
            t.title.clone(),
            String::new(),
            UNJUDGED.into(),
        ];
        (cells, Style::new().dark_gray())
    });
    planned.chain(unjudged).collect()
}

fn project_row(p: &ProjectLine) -> (Vec<String>, Style) {
    let (on, style) = match p.enabled {
        true => ("on", Style::new().green()),
        false => ("off", Style::new()),
    };
    let waiting = format!("{}/{}", p.waiting, p.review_cap);
    (
        vec![on.into(), p.name.clone(), p.device.clone(), waiting],
        style,
    )
}

fn worker_rows(lines: &[WorkerLine], now_ms: UnixMs) -> Rows {
    lines
        .iter()
        .map(|w| {
            let style = match w.state {
                RunState::Stopped | RunState::NeedsYou => Style::new().yellow(),
                RunState::Starting | RunState::Running | RunState::Checking => Style::new().green(),
                RunState::Rejected => Style::new().red(),
                _ => Style::new(),
            };
            let cells = vec![
                w.label.clone(),
                w.project.clone(),
                w.task.clone(),
                w.model.clone(),
                rel(w.start_ms, now_ms),
                w.summary.clone(),
            ];
            (cells, style)
        })
        .collect()
}

fn pct(p: Option<f64>) -> String {
    p.map_or_else(|| "-".into(), |p| format!("{p:.1}%"))
}

fn budget_rows(v: &DispatcherView, now_ms: UnixMs) -> Rows {
    let mut rows = Vec::new();
    for b in &v.budgets {
        for (i, day) in b.days.iter().enumerate() {
            let when = match i {
                0 => "today".to_string(),
                _ => format!("{}d ago", (now_ms - day.day_start_ms).max(0) / DAY_MS),
            };
            let style = if i == 0 {
                Style::new()
            } else {
                Style::new().dark_gray()
            };
            let cells = vec![
                b.account.clone(),
                when,
                pct(day.spent_pct),
                pct(day.spendable_pct),
                pct(day.left_pct),
            ];
            rows.push((cells, style));
        }
    }
    rows
}

fn value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The coordinator's own limits, then each device's read-only ones (A23).
fn limit_rows(d: &Snapshot, v: &DispatcherView) -> Rows {
    let own = d.limits.iter().map(|s| {
        let set_in = if s.from_file {
            "config.toml"
        } else {
            "default"
        };
        (
            vec![
                s.key.clone(),
                value(&s.value),
                value(&s.default),
                set_in.into(),
            ],
            Style::new(),
        )
    });
    let devices = v.devices.iter().flat_map(|dev| {
        dev.limits.iter().map(|s| {
            let cells = vec![
                s.key.clone(),
                value(&s.value),
                value(&s.default),
                format!("{} read-only", dev.id),
            ];
            (cells, Style::new().dark_gray())
        })
    });
    own.chain(devices).collect()
}

fn details(d: &Snapshot, v: &DispatcherView, pane: Pane, sel: Option<usize>) -> Vec<Line<'static>> {
    let i = sel.unwrap_or(0);
    let mut lines = match pane {
        Pane::Workers => v.workers.get(i).map(|w| worker_detail(w, false)),
        Pane::Review => v.review.get(i).map(|w| worker_detail(w, true)),
        Pane::Launch => Some(launch_detail(v, i)),
        Pane::Needs => v.needs_you.get(i).map(|n| {
            vec![
                Line::from(n.subject.clone()).bold(),
                Line::from(n.reason.clone()),
            ]
        }),
        Pane::Budget => budget_detail(v, i),
        Pane::Projects => Some(vec![Line::from(
            "t switches the selected project; on asks first, off does not.",
        )]),
        Pane::Devices => Some(vec![Line::from(
            "t switches the selected device; on asks first, off stops its workers, state saved.",
        )]),
        Pane::Limits => Some(limit_detail(d, i)),
        Pane::Rules => open_proposals(d).get(i).map(|p| {
            vec![
                Line::from(p.line.clone()).bold(),
                Line::from(p.reason.clone()),
                Line::from(format!(
                    "from run {}; a approves (asks first), d rejects",
                    p.run
                ))
                .dark_gray(),
            ]
        }),
    }
    .unwrap_or_default();
    if lines.is_empty() {
        lines.push(Line::from("v shows the next pane").dark_gray());
    }
    lines
}

fn worker_detail(w: &WorkerLine, review: bool) -> Vec<Line<'static>> {
    let branch = w.branch.as_deref().unwrap_or("no branch yet");
    let mut lines = vec![
        Line::from(format!("{}: {}", w.label, w.summary)).bold(),
        Line::from(format!("why: {}", w.why)),
        Line::from(format!(
            "{} {} {} | {branch}",
            snake(&w.provider),
            w.account,
            w.model
        ))
        .dark_gray(),
    ];
    if review {
        let run = w.run;
        lines.push(
            Line::from(format!(
                "k keeps, M merges, D discards (each asks first); or idlemax review keep|discard {run}"
            ))
            .cyan(),
        );
    }
    lines
}

fn launch_detail(v: &DispatcherView, i: usize) -> Vec<Line<'static>> {
    let mut lines = match v.planned.get(i) {
        Some(p) => vec![
            Line::from(p.title.clone()).bold(),
            Line::from(p.reason.clone()),
            Line::from(format!(
                "{} {} {} on {}",
                snake(&p.provider),
                p.account,
                p.model,
                p.device
            ))
            .dark_gray(),
        ],
        None => v
            .ready_unjudged
            .get(i - v.planned.len().min(i))
            .map(|t| vec![Line::from(t.title.clone()).bold(), Line::from(UNJUDGED)])
            .unwrap_or_default(),
    };
    lines.push(Line::from(v.picked_by.clone()).dark_gray());
    lines
}

fn budget_detail(v: &DispatcherView, i: usize) -> Option<Vec<Line<'static>>> {
    let (b, day) = v
        .budgets
        .iter()
        .flat_map(|b| b.days.iter().map(move |d| (b, d)))
        .nth(i)?;
    let why = day
        .why
        .clone()
        .unwrap_or_else(|| "no reason recorded".into());
    Some(vec![
        Line::from(format!("{} ({})", b.account, snake(&b.provider))).bold(),
        Line::from(why),
    ])
}

fn limit_detail(d: &Snapshot, i: usize) -> Vec<Line<'static>> {
    let text = if i < d.limits.len() {
        "e edits the selected limit: type the value, Enter saves, the word default puts it back."
    } else {
        "read-only: change it on that device with idlemax config set."
    };
    vec![Line::from(text)]
}

/// The footer while a limit is typed.
pub fn input_text(input: &LimitInput) -> String {
    format!(
        "{} = {}_  (Enter saves, `default` resets, Esc closes)",
        input.key, input.text
    )
}

/// The Dispatch keys worth a hint on `pane`.
pub fn pane_keys(pane: Pane) -> &'static str {
    match pane {
        Pane::Projects => "v pane  t switch  m mode  g start",
        Pane::Devices => "v pane  t switch  m mode  g start",
        Pane::Limits => "v pane  e edit  m mode  g start",
        Pane::Rules => "v pane  a/d approve/reject",
        Pane::Review => "v pane  k keep  M merge  D discard",
        _ => "v pane  m mode  g start",
    }
}
