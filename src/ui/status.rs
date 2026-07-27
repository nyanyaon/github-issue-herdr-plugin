//! The status line: one line carrying every failure and empty state.
//!
//! There are no modals, and a failure never clears what is already on screen.

use std::fmt;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::github::ApiError;
use crate::identity::IdentityError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLine {
    /// The workspace could not be resolved to a repo, or the repo to a slug.
    Identity(IdentityError),
    /// The API refused, or could not be reached.
    Api(ApiError),
    /// The query succeeded and the repo has no open issues.
    NoOpenIssues,
}

impl fmt::Display for StatusLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(error) => error.fmt(f),
            Self::Api(error) => error.fmt(f),
            Self::NoOpenIssues => write!(f, "no open issues"),
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, status: &StatusLine) {
    let line = Line::from(format!(" {status}")).style(Style::default().fg(Color::Yellow));
    frame.render_widget(line, area);
}
