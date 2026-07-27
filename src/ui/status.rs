//! The status line: one line carrying every failure and empty state.
//!
//! There are no modals, and a failure never clears what is already on screen.

use std::fmt;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::config::{self, ConfigWarning};
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
    /// A cold start: nothing is cached for this repo, and the list query it was
    /// opened with has not answered yet.
    ///
    /// The one line a pane with no data can honestly show. A warm start never
    /// shows it — there the rows are already up, with their age in the header,
    /// and SPEC §11 is explicit that nothing spins over content that exists.
    Fetching,
    /// `config.toml` was read and something in it was ignored.
    ///
    /// The least urgent line there is — nothing is broken, and the defaults are
    /// already in force — so it shows whenever nothing else needs the line, and
    /// keeps showing for the pane's lifetime. The config is read once at
    /// startup, so the warning stays true until the pane is reopened.
    Config(Vec<ConfigWarning>),
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
            Self::Fetching => write!(f, "fetching issues · nothing cached for this repo yet"),
            // The file is named once, then every clause it earned.
            Self::Config(warnings) => {
                let clauses: Vec<String> = warnings.iter().map(ToString::to_string).collect();
                write!(f, "{} · {}", config::FILE_NAME, clauses.join(" · "))
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, status: &StatusLine) {
    let line = Line::from(format!(" {status}")).style(Style::default().fg(Color::Yellow));
    frame.render_widget(line, area);
}
