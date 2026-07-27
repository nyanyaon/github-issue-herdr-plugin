//! The status line: one line carrying every failure and empty state.
//!
//! There are no modals, and a failure never clears what is already on screen.

use std::fmt;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::github::{ApiError, IssueStates};
use crate::identity::IdentityError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLine {
    /// The workspace could not be resolved to a repo, or the repo to a slug.
    Identity(IdentityError),
    /// The API refused, or could not be reached.
    Api(ApiError),
    /// The query succeeded and returned nothing for the state it asked for.
    Empty(IssueStates),
}

impl fmt::Display for StatusLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(error) => error.fmt(f),
            Self::Api(error) => error.fmt(f),
            // Each empty state names the state `o` would move to next, so the
            // way out of an empty list is on screen.
            Self::Empty(IssueStates::Open) => {
                write!(f, "no open issues · [o] to include closed")
            }
            Self::Empty(IssueStates::Closed) => {
                write!(f, "no closed issues · [o] to include all")
            }
            Self::Empty(IssueStates::All) => write!(f, "no issues"),
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, status: &StatusLine) {
    let line = Line::from(format!(" {status}")).style(Style::default().fg(Color::Yellow));
    frame.render_widget(line, area);
}
