//! The detail view: the issue's facts, its body, and its comment thread.
//!
//! Same chrome as the list view — two header lines, a rule, the content, a
//! rule, the status line — so drilling in never moves the furniture. The
//! content is laid out against the pane's current width on every draw, so a
//! `SIGWINCH` re-wraps it and nothing else.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::age;
use crate::app::App;
use crate::github::{IssueComment, IssueDetail};
use crate::ui::{markdown, status, truncate};

/// The keys this view binds. `m` (load more comments) and `r` (re-fetch this
/// issue) are later tickets, so they are not advertised.
const KEY_HINTS: &str = " esc back   j/k scroll   n/p next issue   q close";

/// A one-column left margin, so prose never starts against the pane edge.
const MARGIN: u16 = 1;

pub fn render(frame: &mut Frame, app: &App) {
    let Some(detail) = app.detail() else {
        return;
    };
    let [title, meta, top_rule, content, bottom_rule, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let now = age::now();
    render_title(frame, title, detail);
    render_meta(frame, meta, detail, now);
    render_rule(frame, top_rule);
    render_content(frame, content, app, detail, now);
    render_rule(frame, bottom_rule);
    match app.status() {
        Some(line) => status::render(frame, footer, line),
        None => frame.render_widget(
            Line::from(KEY_HINTS).style(Style::default().fg(Color::DarkGray)),
            footer,
        ),
    }
}

/// ` ‹ #8 Packaging and distribution for a publishable Rust plugin`.
///
/// The `‹` is the way back — `esc` returns to the list with its selection.
fn render_title(frame: &mut Frame, area: Rect, detail: &IssueDetail) {
    let text = format!(" ‹ #{} {}", detail.number, detail.title);
    let line = Line::from(truncate(&text, area.width as usize))
        .style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(line, area);
}

/// ` open · grilling · nyanyaon · 18m ago · 1 comment        fetched just now`.
fn render_meta(frame: &mut Frame, area: Rect, detail: &IssueDetail, now: i64) {
    let mut facts = vec![detail.state.to_lowercase()];
    if let Some(label) = detail.first_label() {
        facts.push(label.to_string());
    }
    if let Some(author) = &detail.author {
        facts.push(author.clone());
    }
    if let Some(updated_at) = detail.updated_at {
        facts.push(age::ago_phrase(updated_at, now));
    }
    facts.push(comment_count(detail.comment_total_count));

    let left = format!(" {}", facts.join(" · "));
    let right = age::fetched_phrase(detail.fetched_at, now);
    let width = area.width as usize;
    let dim = Style::default().fg(Color::DarkGray);

    // The data age only earns its place when the facts leave room for it.
    let spans = if left.chars().count() + right.chars().count() + 2 <= width {
        let gap = width - left.chars().count() - right.chars().count() - 1;
        vec![
            Span::styled(left, dim),
            Span::raw(" ".repeat(gap)),
            Span::styled(right, dim),
        ]
    } else {
        vec![Span::styled(truncate(&left, width), dim)]
    };
    frame.render_widget(Line::from(spans), area);
}

fn comment_count(total: u64) -> String {
    match total {
        0 => "no comments".to_string(),
        1 => "1 comment".to_string(),
        many => format!("{many} comments"),
    }
}

fn render_rule(frame: &mut Frame, area: Rect) {
    let rule = "─".repeat(area.width as usize);
    frame.render_widget(
        Line::from(rule).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_content(frame: &mut Frame, area: Rect, app: &App, detail: &IssueDetail, now: i64) {
    let inner = Rect {
        x: area.x + MARGIN,
        width: area.width.saturating_sub(MARGIN),
        ..area
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let lines = content_lines(detail, inner.width as usize, now);
    // Scrolling is clamped against the content as it is at this width, so a
    // resize can shorten the view without stranding it past the end.
    let height = inner.height as usize;
    app.clamp_detail_scroll(lines.len().saturating_sub(height));
    let offset = app.detail_scroll();

    let visible: Vec<Line> = lines.into_iter().skip(offset).take(height).collect();
    frame.render_widget(Text::from(visible), inner);
}

/// The body, then the comments in the order they were written, each behind its
/// own author-and-timestamp rule.
fn content_lines(detail: &IssueDetail, width: usize, now: i64) -> Vec<Line<'static>> {
    let mut lines = markdown::render(&detail.body, width);
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no description",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for comment in &detail.comments {
        lines.push(Line::default());
        lines.push(comment_rule(comment, width, now));
        lines.extend(markdown::render(&comment.body, width));
    }
    lines
}

/// `── nyanyaon · 18m ago ─────────────────────────────`.
fn comment_rule(comment: &IssueComment, width: usize, now: i64) -> Line<'static> {
    let author = comment.author.as_deref().unwrap_or("ghost");
    let when = comment
        .created_at
        .map(|created_at| age::ago_phrase(created_at, now))
        .unwrap_or_default();
    let label = if when.is_empty() {
        format!("── {author} ")
    } else {
        format!("── {author} · {when} ")
    };
    let fill = width.saturating_sub(label.chars().count());
    let rule = format!("{label}{}", "─".repeat(fill));
    Line::from(Span::styled(
        truncate(&rule, width),
        Style::default().fg(Color::DarkGray),
    ))
}
