//! Delivery: the "why is this job not running" parts of the Jobs tab: a headline for the
//! RESULT column and a detail pane with every reason and its fix.

use idlemax_core::explain::{Explanation, WhySummary};
use idlemax_core::types::{Job, JobId, JobStatus};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::layout::{ellipsize, wrap_indented};
use crate::cli::why;

/// Jobs whose reasons are worth asking for: waiting, held on a device, or parked.
pub fn explainable(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Queued | JobStatus::Leased | JobStatus::Paused | JobStatus::Parked
    )
}

/// What the detail pane knows about one job.
#[derive(Debug)]
pub struct WhyState {
    pub job: JobId,
    pub result: Result<Explanation, String>,
    /// First visible line; the pane clamps it to what exists.
    pub scroll: usize,
}

/// One line for the RESULT / WHY column: the headline reason and how many more the pane
/// lists, e.g. "on battery (guard pause) +2 more". None when nothing holds the job.
/// The device is left out (the pane lists those) and so is a "waiting" tag (STATUS says it).
pub fn headline(summary: &WhySummary) -> Option<String> {
    let head = summary.headline.as_ref()?;
    Some(match summary.more {
        0 => head.reason.message.clone(),
        n => format!("{} +{n} more", head.reason.message),
    })
}

/// Draws the pane for `job` and keeps its scroll position inside the text.
pub fn render_pane(f: &mut Frame, area: Rect, job: Option<&Job>, why: &mut Option<WhyState>) {
    let width = usize::from(area.width).saturating_sub(2);
    let height = usize::from(area.height).saturating_sub(2);
    let shown = why.as_mut().filter(|w| Some(w.job) == job.map(|j| j.id));
    let lines = content(job, shown.as_deref(), width);
    let scroll = match shown {
        Some(w) => {
            w.scroll = w.scroll.min(lines.len().saturating_sub(height));
            w.scroll
        }
        None => 0,
    };
    let more = if lines.len() > height {
        let last = (scroll + height).min(lines.len());
        format!(" - {}-{last} of {}, PgUp/PgDn ", scroll + 1, lines.len())
    } else {
        " ".to_string()
    };
    let title = match job {
        Some(j) => format!(" why job {} ({}){more}", j.id, ellipsize(&j.spec.name, 24)),
        None => " why ".to_string(),
    };
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    f.render_widget(
        Paragraph::new(visible).block(Block::bordered().title(title)),
        area,
    );
}

fn content(job: Option<&Job>, why: Option<&WhyState>, width: usize) -> Vec<Line<'static>> {
    let plain = |text: String, style: Style| -> Vec<Line<'static>> {
        wrap_indented(&text, width)
            .into_iter()
            .map(|l| Line::from(l).style(style))
            .collect()
    };
    let Some(job) = job else {
        return plain("No job selected.".into(), Style::new().dark_gray());
    };
    if !explainable(job.status) {
        let text = format!(
            "Job {} is {}. w explains jobs that are queued, leased, paused or parked.",
            job.id, job.status
        );
        return plain(text, Style::new().dark_gray());
    }
    match why.map(|w| &w.result) {
        None => plain("loading...".into(), Style::new().dark_gray()),
        Some(Err(e)) => {
            let mut lines = plain(
                format!("could not load the explanation: {e}"),
                Style::new().red(),
            );
            lines.extend(plain(
                "It tries again on the next refresh.".into(),
                Style::new().dark_gray(),
            ));
            lines
        }
        Some(Ok(explanation)) => explanation_lines(explanation, width),
    }
}

/// The same text as `idlemax why`, with the summary and section titles bold and fixes cyan.
fn explanation_lines(e: &Explanation, width: usize) -> Vec<Line<'static>> {
    why::render(e)
        .split('\n')
        .enumerate()
        .flat_map(|(i, source)| {
            let style = if i == 0 || (source.ends_with(':') && !source.starts_with(' ')) {
                Style::new().bold()
            } else if source.trim_start().starts_with("fix:") {
                Style::new().cyan()
            } else {
                Style::new()
            };
            wrap_indented(source, width)
                .into_iter()
                .map(move |l| Line::from(l).style(style))
        })
        .collect()
}
