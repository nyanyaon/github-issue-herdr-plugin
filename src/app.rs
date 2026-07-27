//! The app seam.
//!
//! Construct from an [`Environment`], feed key events, render into any terminal
//! backend. Tests drive exactly this and assert on the rendered text; `main`
//! drives it with a real PTY. Nothing below the seam is substitutable except the
//! GraphQL endpoint.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;

use crate::environment::Environment;
use crate::github::{ApiError, GithubClient, IssueDetail, IssueList, IssueRow};
use crate::identity::RepoIdentity;
use crate::ui;
use crate::ui::status::StatusLine;

/// How many issues the list query asks for. The config file that makes this
/// tunable is a later ticket.
const LIST_PAGE_SIZE: u32 = 50;

/// How many comments one detail query asks for — GraphQL's maximum, and the
/// whole thread for all but the longest issues. Paging past it is a later
/// ticket.
const DETAIL_COMMENT_PAGE_SIZE: u32 = 100;

/// The two screens. There is no third.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    List,
    Detail,
}

pub struct App {
    identity: Option<RepoIdentity>,
    /// The client, kept for the detail fetches `enter`, `n` and `p` make. It
    /// holds no connection and does no work between calls.
    client: Option<GithubClient>,
    issue_list: Option<IssueList>,
    detail: Option<IssueDetail>,
    status: Option<StatusLine>,
    view: View,
    selected: usize,
    /// The detail view's first visible line. A [`Cell`] because the draw is the
    /// only place the content's height is known, and it clamps this there.
    detail_scroll: Cell<usize>,
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
            client: None,
            issue_list: None,
            detail: None,
            status: None,
            view: View::List,
            selected: 0,
            detail_scroll: Cell::new(0),
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
        app.client = Some(client);
        app
    }

    /// The keys the list view binds: `j`/`k`, the arrows, `g`/`G`, `enter`, `q`.
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
        if self.view == View::Detail {
            self.handle_detail_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.last_row(),
            KeyCode::Enter => self.open(self.selected),
            KeyCode::Char('q') => self.exit = true,
            _ => {}
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        match self.view {
            View::List => ui::list::render(frame, self),
            View::Detail => ui::detail::render(frame, self),
        }
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

    /// The issue on screen in the detail view, if that is the view on screen.
    pub fn detail(&self) -> Option<&IssueDetail> {
        self.detail.as_ref()
    }

    /// The detail view's first visible line.
    pub fn detail_scroll(&self) -> usize {
        self.detail_scroll.get()
    }

    /// Holds the scroll inside content of this height, at the draw that is the
    /// first to know it. A resize therefore re-wraps and re-clamps together.
    pub fn clamp_detail_scroll(&self, max: usize) {
        self.detail_scroll.set(self.detail_scroll.get().min(max));
    }

    /// `esc` back, `j`/`k` scroll, `n`/`p` next and previous issue, `q` close.
    ///
    /// `n` and `p` follow the list's order and stay in the detail view, so a
    /// run of issues reads without a trip back to the list.
    fn handle_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.view = View::List;
                self.detail = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.detail_scroll.set(self.detail_scroll.get() + 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.detail_scroll
                    .set(self.detail_scroll.get().saturating_sub(1));
            }
            KeyCode::Char('n') => self.step(1),
            KeyCode::Char('p') => self.step(-1),
            KeyCode::Char('q') => self.exit = true,
            _ => {}
        }
    }

    /// Opens the issue one step along the list from the one on screen.
    ///
    /// The selection only moves if the fetch succeeded, so a failure leaves the
    /// issue being read exactly where it was.
    fn step(&mut self, delta: isize) {
        let Some(target) = self.selected.checked_add_signed(delta) else {
            return;
        };
        if target >= self.rows().len() {
            return;
        }
        self.open(target);
    }

    /// Fetches one issue's detail and shows it — the only thing besides startup
    /// that touches the network, and only ever because a key asked it to.
    ///
    /// On failure the status line says why and the view does not change: a
    /// failed fetch never clears what is already on screen.
    fn open(&mut self, index: usize) {
        let Some(row) = self.rows().get(index) else {
            return;
        };
        let number = row.number;
        let (Some(client), Some(identity)) = (self.client.as_ref(), self.identity.as_ref()) else {
            return;
        };
        match client.issue_detail(&identity.slug, number, DETAIL_COMMENT_PAGE_SIZE) {
            Ok(detail) => {
                self.detail = Some(detail);
                self.detail_scroll.set(0);
                self.selected = index;
                self.view = View::Detail;
                self.status = None;
            }
            Err(error) => self.status = Some(StatusLine::Api(error)),
        }
    }

    /// The rows `enter`, `n` and `p` move through, in the list's order.
    fn rows(&self) -> &[IssueRow] {
        self.issue_list
            .as_ref()
            .map(|list| list.rows.as_slice())
            .unwrap_or_default()
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
