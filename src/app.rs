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
use crate::github::{ApiError, GithubClient, IssueDetail, IssueList, IssueRow, IssueStates};
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

/// What the keyboard means right now.
///
/// `/` switches to [`Mode::Filtering`], where ordinary letters are filter text
/// rather than commands, and `esc` or `enter` leaves it again. The mode is
/// explicit state precisely so that the list's plain-key commands and the
/// filter's letters never have to guess which of them a keystroke was meant for.
///
/// Orthogonal to [`View`]: the mode only decides what a key means *in the list*,
/// and the detail view reads its own keys before the mode is ever consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain keys are commands.
    Normal,
    /// Plain keys are filter text.
    Filtering,
}

pub struct App {
    identity: Option<RepoIdentity>,
    /// Held for the pane's lifetime so that `o` can re-query, and `enter`, `n`
    /// and `p` can fetch a detail, without resolving the identity or the token
    /// again. It holds no connection and does no work between calls.
    client: Option<GithubClient>,
    states: IssueStates,
    issue_list: Option<IssueList>,
    detail: Option<IssueDetail>,
    /// The filter text. Applied to the cached rows at render time; changing it
    /// never touches the network.
    filter: String,
    mode: Mode,
    status: Option<StatusLine>,
    view: View,
    /// An index into the *visible* rows, not into the fetched list.
    selected: usize,
    /// The detail view's first visible line. A [`Cell`] because the draw is the
    /// only place the content's height is known, and it clamps this there.
    detail_scroll: Cell<usize>,
    exit: bool,
}

impl App {
    /// Resolves the repo identity, then issues the issue list query — once.
    ///
    /// After this returns the process holds no timer, thread or subscription: it
    /// blocks on terminal input between renders, and the only thing that can
    /// make it query again is a keystroke asking it to (`o`).
    pub fn start(environment: &Environment) -> Self {
        let mut app = Self {
            identity: None,
            client: None,
            states: IssueStates::default(),
            issue_list: None,
            detail: None,
            filter: String::new(),
            mode: Mode::Normal,
            status: None,
            view: View::List,
            selected: 0,
            detail_scroll: Cell::new(0),
            exit: false,
        };

        let identity = match crate::identity::resolve(environment) {
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

        app.client = Some(GithubClient::new(environment.graphql_url.clone(), token));
        let _ = app.refresh_list();
        app
    }

    /// The keys the list view binds: `j`/`k`, the arrows, `g`/`G`, `enter`, `/`,
    /// `esc`, `o`, `q`. The detail view's own keys are in
    /// [`App::handle_detail_key`].
    ///
    /// Anything carrying Control or Alt is ignored outright, so `ctrl+b` and
    /// `ctrl+v` — herdr's prefix and its image paste — are never consumed.
    ///
    /// While [`Mode::Filtering`] every plain key is filter text, so the commands
    /// below are only read in [`Mode::Normal`].
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
        // The detail view reads first: it is a whole screen, and the filter's
        // typing mode only ever governs the list underneath it.
        if self.view == View::Detail {
            self.handle_detail_key(key);
            return;
        }
        if self.mode == Mode::Filtering {
            self.handle_filter_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => self.selected = self.last_row(),
            KeyCode::Enter => self.open(self.selected),
            KeyCode::Char('/') => self.mode = Mode::Filtering,
            KeyCode::Esc => self.clear_filter(),
            KeyCode::Char('o') => self.cycle_states(),
            KeyCode::Char('q') => self.exit = true,
            _ => {}
        }
    }

    /// Typing mode. Every arm here works on cached rows alone — no key in this
    /// match can reach the network, which is what makes typing unstallable.
    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            // `esc` clears the filter and restores the full list.
            KeyCode::Esc => self.clear_filter(),
            // `enter` keeps the filter but hands the plain keys back to the list.
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp_selection();
            }
            // The arrows still move, so a filter can be narrowed and walked
            // without leaving typing mode. `j` and `k` are text here.
            KeyCode::Down => self.select_next(),
            KeyCode::Up => self.select_previous(),
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.clamp_selection();
            }
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

    /// An index into [`App::visible_rows`].
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
        if target >= self.visible_rows().len() {
            return;
        }
        self.open(target);
    }

    /// Fetches one issue's detail and shows it — the only thing besides startup
    /// that touches the network, and only ever because a key asked it to.
    ///
    /// On failure the status line says why and the view does not change: a
    /// failed fetch never clears what is already on screen.
    /// `index` counts the *visible* rows, so `enter` under a filter opens the
    /// issue that is actually under the cursor rather than the nth fetched one.
    fn open(&mut self, index: usize) {
        let Some(number) = self.visible_rows().get(index).map(|row| row.number) else {
            return;
        };
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

    /// The rows the filter lets through, in list order — and the rows `enter`,
    /// `n` and `p` move through.
    ///
    /// Cheap enough to recompute per render — fifty rows and a subsequence test —
    /// so there is no second copy of the list to keep in step with the first.
    pub fn visible_rows(&self) -> Vec<&IssueRow> {
        let Some(list) = self.issue_list.as_ref() else {
            return Vec::new();
        };
        list.rows
            .iter()
            .filter(|row| matches_filter(row, &self.filter))
            .collect()
    }

    /// The row under the selection, once the filter has been applied.
    pub fn selected_row(&self) -> Option<&IssueRow> {
        self.visible_rows().get(self.selected).copied()
    }

    /// The filter text, empty when no filter is active.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Whether plain keys are currently filter text rather than commands.
    pub fn is_filtering(&self) -> bool {
        self.mode == Mode::Filtering
    }

    /// Which issues the list on screen was queried for.
    pub fn states(&self) -> IssueStates {
        self.states
    }

    /// `o`: the next state, and the one list query that goes with it.
    fn cycle_states(&mut self) {
        let previous = self.states;
        self.states = self.states.cycled();
        if !self.refresh_list() {
            // The rows on screen are still the previous state's — the failure is
            // a status line, not an empty pane — so the header has to keep
            // describing them.
            self.states = previous;
        }
    }

    /// The only place the issue list is queried. Cache-first: a failure becomes
    /// a status line and leaves the rows already on screen exactly as they are.
    ///
    /// Answers whether the list on screen is now the one for [`App::states`].
    fn refresh_list(&mut self) -> bool {
        let (Some(client), Some(identity)) = (self.client.as_ref(), self.identity.as_ref()) else {
            return false;
        };
        let result = client.issue_list(&identity.slug, self.states, LIST_PAGE_SIZE);
        let fetched = match result {
            Ok(list) => {
                self.status = list
                    .rows
                    .is_empty()
                    .then_some(StatusLine::Empty(self.states));
                self.issue_list = Some(list);
                true
            }
            Err(error) => {
                self.status = Some(StatusLine::Api(error));
                false
            }
        };
        self.clamp_selection();
        fetched
    }

    fn clear_filter(&mut self) {
        self.filter.clear();
        self.mode = Mode::Normal;
        self.clamp_selection();
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(self.last_row());
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Keeps the selection on a row that exists after the visible set shrinks —
    /// or on the first row when the filter matches nothing at all.
    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.last_row());
    }

    fn last_row(&self) -> usize {
        self.visible_rows().len().saturating_sub(1)
    }
}

/// Does one row survive the filter?
///
/// The query is matched against the number, the title, every label and the
/// author at once, so `42`, `#42`, `walking`, `map` and `nyanyaon` all find
/// something without a mode or a prefix. Matching is fuzzy per ADR-0002: the
/// query's characters must appear in the field in order, but need not be
/// adjacent, so `wskel` finds `Walking skeleton`. It is case-insensitive, and a
/// short query deliberately keeps a lot of rows — the next keystroke narrows it,
/// and rows stay in `updatedAt` order rather than being reordered by score.
fn matches_filter(row: &IssueRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    let field_matches = |field: &str| is_subsequence(&query, &field.to_lowercase());

    field_matches(&format!("#{}", row.number))
        || field_matches(&row.title)
        || row.labels.iter().any(|label| field_matches(label))
        || row.author.as_deref().is_some_and(field_matches)
}

/// Do `query`'s characters occur in `field`, in order but not necessarily
/// adjacent? Both sides are already lowercased by the caller.
fn is_subsequence(query: &str, field: &str) -> bool {
    let mut field = field.chars();
    query
        .chars()
        .all(|wanted| field.any(|character| character == wanted))
}
