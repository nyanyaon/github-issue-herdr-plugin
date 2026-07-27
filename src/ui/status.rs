//! The status line: one line carrying every failure and empty state.
//!
//! There are no modals, and a failure never clears what is already on screen.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::age;
use crate::config::{self, ConfigWarning};
use crate::github::{ApiError, IssueStates};
use crate::identity::IdentityError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLine {
    /// The workspace could not be resolved to a repo, or the repo to a slug.
    Identity(IdentityError),
    /// The API refused, or could not be reached.
    Api(ApiError),
    /// The API could not be reached, over data that is still on screen.
    ///
    /// Its own variant rather than an [`ApiError`] because the age belongs to
    /// the *cache*, which the client below this knows nothing about: the client
    /// reports that it could not reach GitHub, and the age of what the user is
    /// left reading is joined to it here. Build it with [`StatusLine::api`],
    /// which is the only thing that decides between this and `Api`.
    Offline {
        /// When the data on screen was fetched, or `None` on a cold start with
        /// nothing cached — where "showing cache from" would be a lie.
        cached_at: Option<i64>,
    },
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

impl StatusLine {
    /// The line one API failure earns, over data fetched at `cached_at`.
    ///
    /// The one place [`StatusLine::Offline`] is built, so a transport failure
    /// can only ever reach the screen with the age of what it left there.
    pub fn api(error: ApiError, cached_at: Option<i64>) -> Self {
        match error {
            ApiError::Offline => Self::Offline { cached_at },
            error => Self::Api(error),
        }
    }

    /// The line's text as of `now`.
    ///
    /// `now` is a parameter rather than a clock read here because SPEC §12 has
    /// ages computed at render time only: the offline line's age moves, and it
    /// moves when the pane draws, never on a timer.
    pub fn text(&self, now: i64) -> String {
        match self {
            Self::Identity(error) => error.to_string(),
            Self::Api(error) => error.to_string(),
            // SPEC §11 verbatim: what is on screen is the cache, and the line
            // says how old it is. With nothing cached there is no age to state,
            // and the client's own wording is the honest one.
            Self::Offline { cached_at } => match cached_at {
                Some(cached_at) => {
                    format!(
                        "offline · showing cache from {}",
                        age::ago_phrase(*cached_at, now)
                    )
                }
                None => ApiError::Offline.to_string(),
            },
            // Each empty state names the state `o` would move to next, so the
            // way out of an empty list is on screen.
            Self::Empty(IssueStates::Open) => "no open issues · [o] to include closed".to_string(),
            Self::Empty(IssueStates::Closed) => "no closed issues · [o] to include all".to_string(),
            Self::Empty(IssueStates::All) => "no issues".to_string(),
            Self::Fetching => "fetching issues · nothing cached for this repo yet".to_string(),
            // The file is named once, then every clause it earned.
            Self::Config(warnings) => {
                let clauses: Vec<String> = warnings.iter().map(ToString::to_string).collect();
                format!("{} · {}", config::FILE_NAME, clauses.join(" · "))
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, status: &StatusLine) {
    let line = Line::from(format!(" {}", status.text(age::now())))
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(line, area);
}
