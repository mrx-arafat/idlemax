//! Delivery: draws the dashboard from `App` state; no decisions, only layout and color.

use std::collections::HashMap;

use idlemax_core::explain::WhySummary;
use idlemax_core::schedule::{Decision, ScheduleView};
use idlemax_core::types::{
    EventKind, JobId, JobStatus, Recommendation, RecommendationStatus, UnixMs,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, TableState, Tabs, Wrap};
use ratatui::Frame;

use super::app::{App, Tab};
use super::feed::Snapshot;
use super::layout::{ellipsize, fit_hints, join_parts, tab_titles, text_width, wrap_words};
use super::table::{self, empty_state, table};
use super::why::{headline, render_pane, WhyState};
use crate::cli::print::summary_parts;
use crate::cli::rows::{self, rel};

/// The Jobs tab needs at least this many rows to share its space with the reasons pane.
const MIN_PANE_SCREEN: u16 = 12;

pub fn render(f: &mut Frame, app: &mut App, now_ms: UnixMs) {
    let width = usize::from(f.area().width);
    let status = status_text(app, width);
    let footer_text = footer_text(app, width, now_ms);
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1 + rows_for(&status.0)),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(rows_for(&footer_text.0)),
    ])
    .areas(f.area());
    render_header(f, header, app, &status, now_ms);
    render_tabs(f, tabs, app);
    match app.data.take() {
        Some(data) => {
            render_body(f, body, app, &data, now_ms);
            app.data = Some(data);
        }
        None => f.render_widget(
            Paragraph::new("waiting for the coordinator...").block(Block::bordered()),
            body,
        ),
    }
    render_footer(f, footer, footer_text);
    if let Some(c) = &app.confirm {
        render_confirm(f, &c.prompt);
    }
}

/// Screen rows for wrapped `lines`: at least one, at most three.
fn rows_for(lines: &[String]) -> u16 {
    u16::try_from(lines.len().clamp(1, 3)).unwrap_or(1)
}

/// Header lines under the title and whether they are an error: the coordinator
/// error, or the status summary, which drops the version and then wraps between
/// its parts when it is too wide.
fn status_text(app: &App, width: usize) -> (Vec<String>, bool) {
    match (&app.fetch_error, &app.data) {
        (Some(e), _) => (
            wrap_words(&format!("coordinator unreachable: {e}"), width, 3),
            true,
        ),
        (None, Some(d)) => {
            let mut parts = summary_parts(&d.status);
            if text_width(&parts.join(" | ")) > width {
                parts.remove(0);
            }
            (join_parts(&parts, width), false)
        }
        (None, None) => (vec![String::new()], false),
    }
}

fn render_header(
    f: &mut Frame,
    area: Rect,
    app: &App,
    status: &(Vec<String>, bool),
    now_ms: UnixMs,
) {
    let state = match app.data.as_ref().map(|d| d.status.global_paused) {
        Some(true) => Span::styled(
            " GLOBAL PAUSED ",
            Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
        ),
        Some(false) => Span::styled(" running ", Style::new().fg(Color::Black).bg(Color::Green)),
        None if app.fetch_error.is_some() => Span::styled(
            " unreachable ",
            Style::new().fg(Color::Black).bg(Color::Red),
        ),
        None => Span::raw(" connecting "),
    };
    let updated = app
        .updated_ms
        .map_or_else(|| "never".into(), |t| rel(t, now_ms));
    let updated = format!("  updated {updated}");
    // The state and age matter more than the address, so a long address gives way.
    let fixed = "idlemax ".len() + state.width() + 2 + text_width(&updated);
    let source = ellipsize(
        &app.source,
        usize::from(area.width).saturating_sub(fixed).max(12),
    );
    let top = Line::from(vec![
        Span::raw("idlemax ").bold(),
        Span::raw(format!("{source}  ")),
        state,
        Span::raw(updated).dark_gray(),
    ]);
    let (lines, is_error) = status;
    let mut text = vec![top];
    text.extend(lines.iter().map(|l| match is_error {
        true => Line::from(l.clone()).red(),
        false => Line::from(l.clone()),
    }));
    f.render_widget(Paragraph::new(text), area);
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    let counts = Tab::ALL.map(|t| (t.title(), app.rows(t)));
    let titles = tab_titles(&counts, usize::from(area.width));
    let tabs = Tabs::new(titles).select(app.tab.index()).highlight_style(
        Style::new()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    );
    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &mut App, d: &Snapshot, now_ms: UnixMs) {
    let tab = app.tab;
    let zone = app.zone;
    let state = &mut app.tables[tab.index()];
    let why = app.why_open.then_some(&mut app.why);
    match tab {
        Tab::Devices => {
            let rows = d.status.devices.iter().map(|v| {
                let style = match (v.online, v.paused || v.local_paused) {
                    (false, _) => Style::new().dark_gray(),
                    (true, true) => Style::new().yellow(),
                    (true, false) => Style::new(),
                };
                (rows::device_cells(v, now_ms), style)
            });
            table(f, area, &table::DEVICES, rows.collect(), state);
        }
        Tab::Jobs => render_jobs(f, area, d, state, why, now_ms),
        Tab::Quota => {
            let rows = d.quota.iter().map(|q| {
                let paused = d.status.paused_accounts.contains(&q.account);
                let style = if q.spendable && !paused {
                    Style::new().green()
                } else {
                    Style::new()
                };
                (rows::quota_cells(q, paused, now_ms), style)
            });
            table(f, area, &table::QUOTA, rows.collect(), state);
        }
        Tab::Advice => render_advice(f, area, d, state, now_ms),
        Tab::Events => {
            let rows = d.events.iter().map(|e| {
                let style = if e.kind == EventKind::Error {
                    Style::new().red()
                } else {
                    Style::new()
                };
                (rows::event_cells(e, zone), style)
            });
            table(f, area, &table::EVENTS, rows.collect(), state);
        }
        Tab::Schedules => {
            let rows = d
                .schedules
                .iter()
                .map(|v| (rows::schedule_cells(v, now_ms), schedule_style(v)));
            table(f, area, &table::SCHEDULES, rows.collect(), state);
        }
    }
}

/// Paused is yellow, a failed last run red, a job in flight green; otherwise plain.
fn schedule_style(v: &ScheduleView) -> Style {
    let failed = v.last_result.as_ref().is_some_and(|r| {
        matches!(
            r.status,
            JobStatus::Failed | JobStatus::Killed | JobStatus::Parked
        )
    });
    match (v.schedule.paused, v.decision) {
        (true, _) => Style::new().yellow(),
        (false, Decision::Running { .. }) => Style::new().green(),
        _ if failed => Style::new().red(),
        _ => Style::new(),
    }
}

/// The Jobs table. A waiting job's RESULT says why it waits; with the pane open, the
/// selected job's full reasons fill the lower part of the screen.
fn render_jobs(
    f: &mut Frame,
    area: Rect,
    d: &Snapshot,
    state: &mut TableState,
    why: Option<&mut Option<WhyState>>,
    now_ms: UnixMs,
) {
    let summaries: HashMap<JobId, &WhySummary> = d.why.iter().map(|w| (w.job, w)).collect();
    let rows = d.jobs.iter().map(|j| {
        let mut cells = rows::job_cells(j, now_ms);
        let line = summaries.get(&j.id).and_then(|w| headline(w));
        if let (Some(line), Some(result)) = (line, cells.last_mut()) {
            *result = line;
        }
        (cells, job_style(j.status))
    });
    let Some(why) = why.filter(|_| area.height >= MIN_PANE_SCREEN) else {
        return table(f, area, &table::JOBS, rows.collect(), state);
    };
    let wanted = u16::try_from(d.jobs.len() + 3).unwrap_or(u16::MAX);
    let [list, pane] = Layout::vertical([
        Constraint::Length(wanted.min(area.height / 2)),
        Constraint::Min(5),
    ])
    .areas(area);
    table(f, list, &table::JOBS, rows.collect(), state);
    let job = state.selected().and_then(|i| d.jobs.get(i));
    render_pane(f, pane, job, why);
}

fn job_style(s: JobStatus) -> Style {
    match s {
        JobStatus::Running | JobStatus::Leased => Style::new().green(),
        JobStatus::Paused => Style::new().yellow(),
        JobStatus::Failed | JobStatus::Killed | JobStatus::Parked => Style::new().red(),
        JobStatus::Succeeded | JobStatus::Cancelled => Style::new().dark_gray(),
        JobStatus::Queued => Style::new(),
    }
}

fn render_advice(f: &mut Frame, area: Rect, d: &Snapshot, state: &mut TableState, now_ms: UnixMs) {
    if d.advice.is_empty() {
        return empty_state(f, area, table::ADVICE.empty);
    }
    // The list takes what its rows need (at most 55%); the details get the rest.
    let wanted = u16::try_from(d.advice.len() + 3).unwrap_or(u16::MAX);
    let list_height = wanted.min(area.height * 55 / 100);
    let [list, detail] =
        Layout::vertical([Constraint::Length(list_height), Constraint::Min(5)]).areas(area);
    let rows = d.advice.iter().map(|r| {
        let style = match r.status {
            RecommendationStatus::Open => Style::new(),
            _ => Style::new().dark_gray(),
        };
        (rows::advice_cells(r, now_ms), style)
    });
    table(f, list, &table::ADVICE, rows.collect(), state);
    let selected = state.selected().and_then(|i| d.advice.get(i));
    let text = selected.map_or_else(|| vec![Line::from("no advice")], advice_detail);
    let p = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(" details "));
    f.render_widget(p, detail);
}

fn advice_detail(r: &Recommendation) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(r.title.clone()).bold(),
        Line::from(r.why.clone()),
        Line::from(format!(
            "{} | confidence {:.2} | {}",
            rows::snake(&r.kind),
            r.confidence,
            rows::snake(&r.source)
        ))
        .dark_gray(),
    ];
    // What accepting does comes before the evidence: a long list must not push it off screen.
    if let Some(job) = &r.suggested_job {
        lines.push(
            Line::from(format!(
                "job on accept: {} -> {}",
                job.name,
                job.command.join(" ")
            ))
            .cyan(),
        );
    }
    lines.extend(r.evidence.iter().map(|e| Line::from(format!("- {e}"))));
    lines
}

/// Footer lines and their style: a flash message (wrapped, so an error is not cut
/// short) while one is live, else the key hints.
fn footer_text(app: &App, width: usize, now_ms: UnixMs) -> (Vec<String>, Style) {
    match app.live_flash(now_ms) {
        Some(fl) => {
            let style = if fl.error {
                Style::new().red()
            } else {
                Style::new().green()
            };
            (wrap_words(&fl.text, width, 3), style)
        }
        None => (vec![key_hints(app, width)], Style::new().dark_gray()),
    }
}

fn render_footer(f: &mut Frame, area: Rect, (lines, style): (Vec<String>, Style)) {
    let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
    f.render_widget(Paragraph::new(text).style(style), area);
}

/// What the keys do on this tab, trimmed to `width`: `q quit` stays, `P/R all` goes first.
fn key_hints(app: &App, width: usize) -> String {
    // On Advice and Events p/r already act on everything, so P/R would repeat them.
    let (target, global_keys) = match app.tab {
        Tab::Advice | Tab::Events => ("all".to_string(), false),
        Tab::Schedules => ("schedule".to_string(), true),
        _ => {
            let label = app.target().map_or_else(|| "selection".into(), |(_, l)| l);
            (ellipsize(&label, 22), true)
        }
    };
    let mut hints = vec![(1, format!("p/r pause/resume {target}"))];
    match app.tab {
        Tab::Jobs => {
            hints.push((2, "k cancel job".into()));
            hints.push((2, if app.why_open { "w close" } else { "w why" }.into()));
            if app.why_open {
                hints.push((4, "PgUp/PgDn scroll".into()));
            }
        }
        Tab::Advice => hints.push((2, "a accept  d dismiss  f refresh".into())),
        Tab::Schedules => hints.push((2, "d remove".into())),
        _ => {}
    }
    if global_keys {
        hints.push((5, "P/R all".into()));
    }
    hints.push((3, "s stop-all".into()));
    hints.push((4, "tab/1-6 switch".into()));
    hints.push((0, "q quit".into()));
    fit_hints(&hints, width)
}

fn render_confirm(f: &mut Frame, prompt: &str) {
    let width = f.area().width.min(64);
    let prompt_lines = wrap_words(prompt, usize::from(width).saturating_sub(2), 4);
    // Borders, the prompt, a blank line and the hint: the hint must never be cut off.
    let height = u16::try_from(prompt_lines.len() + 4).unwrap_or(u16::MAX);
    let area = f
        .area()
        .centered(Constraint::Length(width), Constraint::Length(height));
    let mut text: Vec<Line> = prompt_lines
        .into_iter()
        .map(|l| Line::from(l).bold())
        .collect();
    text.push(Line::from(""));
    text.push(Line::from("y confirm, any other key cancels"));
    let block = Block::bordered()
        .title(" confirm ")
        .border_style(Style::new().red());
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(text).block(block), area);
}
