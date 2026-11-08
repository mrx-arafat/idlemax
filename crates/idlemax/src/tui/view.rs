//! Delivery: draws the dashboard from `App` state; no decisions, only layout and color.

use idlemax_core::schedule::{Decision, ScheduleView};
use idlemax_core::types::{EventKind, JobStatus, Recommendation, RecommendationStatus, UnixMs};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap};
use ratatui::Frame;

use super::app::{App, Tab};
use super::feed::Snapshot;
use crate::cli::print::summary;
use crate::cli::rows::{self, rel};

pub fn render(f: &mut Frame, app: &mut App, now_ms: UnixMs) {
    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    render_header(f, header, app, now_ms);
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
    render_footer(f, footer, app, now_ms);
    if let Some(c) = &app.confirm {
        render_confirm(f, &c.prompt);
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App, now_ms: UnixMs) {
    let state = match app.data.as_ref().map(|d| d.status.global_paused) {
        Some(true) => Span::styled(
            " GLOBAL PAUSED ",
            Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
        ),
        Some(false) => Span::styled(" running ", Style::new().fg(Color::Black).bg(Color::Green)),
        None => Span::raw(" connecting "),
    };
    let updated = app
        .updated_ms
        .map_or_else(|| "never".into(), |t| rel(t, now_ms));
    let top = Line::from(vec![
        Span::raw("idlemax ").bold(),
        Span::raw(format!("{}  ", app.source)),
        state,
        Span::raw(format!("  updated {updated}")).dark_gray(),
    ]);
    let second = match (&app.fetch_error, &app.data) {
        (Some(e), _) => Line::from(format!("coordinator unreachable: {e}")).red(),
        (None, Some(d)) => Line::from(summary(&d.status)),
        (None, None) => Line::from(""),
    };
    f.render_widget(Paragraph::new(vec![top, second]), area);
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles = Tab::ALL.map(|t| format!("{} {} ({})", t.index() + 1, t.title(), app.rows(t)));
    let tabs = Tabs::new(titles).select(app.tab.index()).highlight_style(
        Style::new()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED),
    );
    f.render_widget(tabs, area);
}

fn render_body(f: &mut Frame, area: Rect, app: &mut App, d: &Snapshot, now_ms: UnixMs) {
    let tab = app.tab;
    let state = &mut app.tables[tab.index()];
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
            table(f, area, &rows::DEVICE_HEADER, rows.collect(), state);
        }
        Tab::Jobs => {
            let rows = d
                .jobs
                .iter()
                .map(|j| (rows::job_cells(j, now_ms), job_style(j.status)));
            table(f, area, &rows::JOB_HEADER, rows.collect(), state);
        }
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
            table(f, area, &rows::QUOTA_HEADER, rows.collect(), state);
        }
        Tab::Advice => render_advice(f, area, d, state, now_ms),
        Tab::Events => {
            let rows = d.events.iter().map(|e| {
                let style = if e.kind == EventKind::Error {
                    Style::new().red()
                } else {
                    Style::new()
                };
                (rows::event_cells(e), style)
            });
            table(f, area, &rows::EVENT_HEADER, rows.collect(), state);
        }
        Tab::Schedules => {
            let rows = d
                .schedules
                .iter()
                .map(|v| (rows::schedule_cells(v, now_ms), schedule_style(v)));
            table(f, area, &rows::SCHEDULE_HEADER, rows.collect(), state);
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

fn job_style(s: JobStatus) -> Style {
    match s {
        JobStatus::Running | JobStatus::Leased => Style::new().green(),
        JobStatus::Paused => Style::new().yellow(),
        JobStatus::Failed | JobStatus::Killed | JobStatus::Parked => Style::new().red(),
        JobStatus::Succeeded | JobStatus::Cancelled => Style::new().dark_gray(),
        JobStatus::Queued => Style::new(),
    }
}

/// Column widths fit the content; the last column takes the rest.
fn table(
    f: &mut Frame,
    area: Rect,
    header: &[&str],
    rows: Vec<(Vec<String>, Style)>,
    state: &mut TableState,
) {
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for (cells, _) in &rows {
        for (w, c) in widths.iter_mut().zip(cells) {
            *w = (*w).max(c.chars().count());
        }
    }
    let last = widths.len().saturating_sub(1);
    let constraints: Vec<Constraint> = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            if i == last {
                Constraint::Fill(1)
            } else {
                Constraint::Length(clamp_u16(*w))
            }
        })
        .collect();
    let head =
        Row::new(header.iter().map(|h| Cell::from(*h))).style(Style::new().bold().underlined());
    let body = rows
        .into_iter()
        .map(|(cells, style)| Row::new(cells).style(style));
    let t = Table::new(body, constraints)
        .header(head)
        .block(Block::bordered())
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2)
        .highlight_symbol("> ");
    f.render_stateful_widget(t, area, state);
}

fn clamp_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn render_advice(f: &mut Frame, area: Rect, d: &Snapshot, state: &mut TableState, now_ms: UnixMs) {
    let [list, detail] =
        Layout::vertical([Constraint::Percentage(55), Constraint::Min(5)]).areas(area);
    let rows = d.advice.iter().map(|r| {
        let style = match r.status {
            RecommendationStatus::Open => Style::new(),
            _ => Style::new().dark_gray(),
        };
        (rows::advice_cells(r, now_ms), style)
    });
    table(f, list, &rows::ADVICE_HEADER, rows.collect(), state);
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
    lines.extend(r.evidence.iter().map(|e| Line::from(format!("- {e}"))));
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
    lines
}

fn render_footer(f: &mut Frame, area: Rect, app: &App, now_ms: UnixMs) {
    let line = match app.live_flash(now_ms) {
        Some(fl) if fl.error => Line::from(fl.text.clone()).red(),
        Some(fl) => Line::from(fl.text.clone()).green(),
        None => Line::from(key_hints(app)).dark_gray(),
    };
    f.render_widget(Paragraph::new(line), area);
}

fn key_hints(app: &App) -> String {
    let target = match (app.tab, app.target()) {
        (Tab::Schedules, _) => "schedule".into(),
        (_, Some((_, label))) => label,
        (_, None) => "selection".into(),
    };
    let extra = match app.tab {
        Tab::Jobs => "  k cancel job",
        Tab::Advice => "  a accept  d dismiss  f refresh",
        Tab::Schedules => "  d remove",
        _ => "",
    };
    format!("p/r pause/resume {target}  P/R all  s stop-all{extra}  tab/1-6 switch  q quit")
}

fn render_confirm(f: &mut Frame, prompt: &str) {
    let area = f
        .area()
        .centered(Constraint::Length(64), Constraint::Length(5));
    let text = vec![
        Line::from(prompt.to_string()).bold(),
        Line::from(""),
        Line::from("y confirm, any other key cancels"),
    ];
    let block = Block::bordered()
        .title(" confirm ")
        .border_style(Style::new().red());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: true }).block(block),
        area,
    );
}
