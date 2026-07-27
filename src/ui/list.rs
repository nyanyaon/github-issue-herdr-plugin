//! The list view: a header, one row per issue, and the status line.
//!
//! Everything is laid out against the pane's current width, so a `SIGWINCH` is
//! nothing more than the next draw at a new size.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::age;
use crate::app::App;
use crate::github::{IssueList, IssueRow};
use crate::ui::{fit, right, status, truncate};

const MARKER_WIDTH: usize = 2;
const NUMBER_WIDTH: usize = 5;
const LABEL_WIDTH: usize = 12;
const COUNT_WIDTH: usize = 3;
const AGE_WIDTH: usize = 4;

/// The keys the list binds, and nothing else. `r` is a later ticket, so it is
/// not advertised. Seventy columns, so it still fits the 72-column pane.
const KEY_HINTS: &str = " j/k move   enter open   g/G top/bottom   / filter   o state   q close";

pub fn render(frame: &mut Frame, app: &App) {
    let [header, top_rule, rows, bottom_rule, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let now = age::now();
    if let Some(list) = app.issue_list() {
        render_header(frame, header, app, list, now);
        render_rows(frame, rows, app, now);
    }
    render_rule(frame, top_rule);
    render_rule(frame, bottom_rule);
    // While typing, the footer is the filter prompt: a user has to see what they
    // are typing, and a status line has nowhere else to go.
    if app.is_filtering() {
        render_filter_prompt(frame, footer, app.filter());
        return;
    }
    match app.status() {
        Some(line) => status::render(frame, footer, line),
        None => frame.render_widget(
            Line::from(KEY_HINTS).style(Style::default().fg(Color::DarkGray)),
            footer,
        ),
    }
}

/// `nyanyaon/github-issue-herdr-plugin · 6 open · fetched 3m ago`, or
/// `nyanyaon/github-issue-herdr-plugin · 3 of 6 shown · fetched 3m ago` while a
/// filter is active.
///
/// The name is always the `nameWithOwner` the API answered with, never the slug
/// parsed from the remote, which is what makes a rename a non-event.
fn render_header(frame: &mut Frame, area: Rect, app: &App, list: &IssueList, now: i64) {
    // The filter counts against the rows on hand, since that is all it can ever
    // match; unfiltered, the count is what the query said the repo holds.
    let counts = if app.filter().is_empty() {
        format!("{} {}", list.total_count, app.states().noun())
    } else {
        format!("{} of {} shown", app.visible_rows().len(), list.rows.len())
    };
    let text = format!(
        " {} · {counts} · {}",
        list.name_with_owner,
        age::fetched_phrase(list.fetched_at, now)
    );
    let line = Line::from(truncate(&text, area.width as usize))
        .style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(line, area);
}

fn render_rule(frame: &mut Frame, area: Rect) {
    let rule = "─".repeat(area.width as usize);
    frame.render_widget(
        Line::from(rule).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// ` /walk` — what has been typed so far, in place of the key hints.
fn render_filter_prompt(frame: &mut Frame, area: Rect, filter: &str) {
    let line = Line::from(truncate(&format!(" /{filter}"), area.width as usize))
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(line, area);
}

/// Draws the rows the filter lets through, which unfiltered is all of them.
fn render_rows(frame: &mut Frame, area: Rect, app: &App, now: i64) {
    let height = area.height as usize;
    if height == 0 {
        return;
    }
    let selected = app.selected();
    // Scrolling is a pure function of the selection and the pane height, so no
    // state survives a resize.
    let offset = selected.saturating_sub(height.saturating_sub(1));
    let lines: Vec<Line> = app
        .visible_rows()
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| row_line(row, index == selected, area.width as usize, now))
        .collect();
    frame.render_widget(ratatui::text::Text::from(lines), area);
}

/// Number, title, first label, comment count when non-zero, relative age.
fn row_line<'a>(row: &IssueRow, selected: bool, width: usize, now: i64) -> Line<'a> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![
        Span::raw(if selected { "▸ " } else { "  " }),
        Span::styled(fit(&format!("#{}", row.number), NUMBER_WIDTH), dim),
        Span::raw(" "),
    ];

    let trailing = COUNT_WIDTH + 3 + AGE_WIDTH;
    let with_label = MARKER_WIDTH + NUMBER_WIDTH + 1 + 1 + LABEL_WIDTH + trailing;
    let without_label = MARKER_WIDTH + NUMBER_WIDTH + 1 + trailing;

    let count = if row.comment_count == 0 {
        String::new()
    } else {
        row.comment_count.to_string()
    };
    let age = row
        .updated_at
        .map(|updated_at| age::relative_age(updated_at, now))
        .unwrap_or_default();

    if width >= with_label + 12 {
        spans.push(title_span(&row.title, width - with_label, selected));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            fit(row.first_label().unwrap_or(""), LABEL_WIDTH),
            dim,
        ));
    } else if width >= without_label + 8 {
        spans.push(title_span(&row.title, width - without_label, selected));
    } else {
        // Too narrow for anything but the number and the title.
        let room = width.saturating_sub(MARKER_WIDTH + NUMBER_WIDTH + 1);
        spans.push(title_span(&row.title, room, selected));
        return Line::from(spans);
    }

    spans.push(Span::styled(right(&count, COUNT_WIDTH), dim));
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(right(&age, AGE_WIDTH), dim));
    Line::from(spans)
}

fn title_span<'a>(title: &str, width: usize, selected: bool) -> Span<'a> {
    let style = if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Span::styled(fit(title, width), style)
}
