//! The app seam.
//!
//! Construct from an [`Environment`], feed key events, render into any terminal
//! backend. Tests drive exactly this and assert on the rendered text; `main`
//! drives it with a real PTY. Nothing below the seam is substitutable except the
//! GraphQL endpoint.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;

use crate::environment::Environment;
use crate::github::{ApiError, GithubClient, IssueList};
use crate::identity::RepoIdentity;
use crate::ui;
use crate::ui::status::StatusLine;

/// How many issues the list query asks for. The config file that makes this
/// tunable is a later ticket.
const LIST_PAGE_SIZE: u32 = 50;

pub struct App {
    identity: Option<RepoIdentity>,
    issue_list: Option<IssueList>,
    status: Option<StatusLine>,
    selected: usize,
    exit: bool,
}

impl App {
    /// Resolves the repo identity, then issues the issue list query — once.
    ///
    /// After this returns the process makes no further network call and holds no
    /// timer, thread or subscription: it blocks on terminal input between
    /// renders.
    pub fn start(environment: &Environment) -> Self {
        let mut app = Self {
            identity: None,
            issue_list: None,
            status: None,
            selected: 0,
            exit: false,
        };

        let identity = match crate::identity::resolve(&environment.workspace_cwd) {
            Ok(identity) => identity,
            Err(error) => {
                app.status = Some(StatusLine::Identity(error));
                return app;
            }
        };
        app.identity = Some(identity);

        let Some(token) = environment.token.clone() else {
            app.status = Some(StatusLine::Api(ApiError::NoToken));
            return app;
        };

        let client = GithubClient::new(environment.graphql_url.clone(), token);
        let slug = &app.identity.as_ref().expect("identity resolved").slug;
        match client.issue_list(slug, LIST_PAGE_SIZE) {
            Ok(list) => {
                if list.rows.is_empty() {
                    app.status = Some(StatusLine::NoOpenIssues);
                }
                app.issue_list = Some(list);
            }
            Err(error) => app.status = Some(StatusLine::Api(error)),
        }
        app
    }

    /// The only keys this slice binds: `j`/`k`, the arrows, `g`/`G`, `q`.
    ///
    /// Anything carrying Control or Alt is ignored outright, so `ctrl+b` and
    /// `ctrl+v` — herdr's prefix and its image paste — are never consumed.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.last_row(),
            KeyCode::Char('q') => self.exit = true,
            _ => {}
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        ui::list::render(frame, self);
    }

    /// Whether `q` has been pressed and the pane should close.
    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn issue_list(&self) -> Option<&IssueList> {
        self.issue_list.as_ref()
    }

    pub fn status(&self) -> Option<&StatusLine> {
        self.status.as_ref()
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.last_row());
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn last_row(&self) -> usize {
        self.issue_list
            .as_ref()
            .map(|list| list.rows.len().saturating_sub(1))
            .unwrap_or(0)
    }
}
