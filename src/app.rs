//! The app seam.
//!
//! Construct from an [`Environment`], feed key events, render into any terminal
//! backend. Tests drive exactly this and assert on the rendered text; `main`
//! drives it with a real PTY. Nothing below the seam is substitutable except the
//! GraphQL endpoint.

use std::cell::Cell;
use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;

use crate::cache::Cache;
use crate::environment::Environment;
use crate::github::{ApiError, GithubClient, IssueDetail, IssueList, IssueRow, IssueStates};
use crate::identity::RepoIdentity;
use crate::ui;
use crate::ui::status::StatusLine;

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
    /// The shared cache, or `None` when there is no state dir to keep one in.
    /// Held open for the pane's lifetime; it costs one file handle and does no
    /// work between calls.
    cache: Option<Cache>,
    /// The list query SPEC §5 step 6 runs *after* the cached rows are on screen.
    ///
    /// It is a flag rather than a call inside [`App::start`] because the frame
    /// between the two is the whole point: the event loop draws, sees this, runs
    /// the query, and draws again. Once it is cleared only a keystroke can set
    /// it again, so the pane goes back to blocking on input.
    pending_list_query: bool,
    /// The detail re-fetch that opening a **stale** issue queues, for the same
    /// reason and through the same seam as the list query above: the cached
    /// issue is drawn first, and the request goes out after that frame.
    ///
    /// Only ever set for an issue the cache could already answer for. An issue
    /// never read has nothing to draw ahead of its request, so it is fetched
    /// there and then instead.
    pending_detail_fetch: Option<u64>,
    states: IssueStates,
    issue_list: Option<IssueList>,
    detail: Option<IssueDetail>,
    /// The `updatedAt` every cached detail of this repo was fetched at, by
    /// issue number — what each list row's own `updatedAt` is compared against.
    ///
    /// Read from the cache when the pane opens and again whenever a detail is
    /// written through, which is the only way it can change. Never a clock:
    /// **staleness is a disagreement between two timestamps GitHub issued**,
    /// and holding the map costs one query per fetch rather than one per row
    /// per render.
    cached_detail_updated_at: HashMap<u64, Option<i64>>,
    /// The filter text. Applied to the cached rows at render time; changing it
    /// never touches the network.
    filter: String,
    mode: Mode,
    status: Option<StatusLine>,
    /// What `config.toml` got wrong, if anything — the line shown whenever
    /// nothing more urgent needs the status line, for the pane's lifetime.
    config_status: Option<StatusLine>,
    view: View,
    /// An index into the *visible* rows, not into the fetched list.
    selected: usize,
    /// The detail view's first visible line. A [`Cell`] because the draw is the
    /// only place the content's height is known, and it clamps this there.
    detail_scroll: Cell<usize>,
    exit: bool,
    /// `list_page_size` and `detail_comment_page_size`, from `config.toml`.
    /// Fixed for the pane's lifetime, like everything else the config decides.
    list_page_size: u32,
    detail_comment_page_size: u32,
}

impl App {
    /// Everything SPEC §5 does *before* the network: the repo identity, the
    /// cache, the token, and the cached rows put on screen.
    ///
    /// It issues no request. The app it returns is the first frame a user sees,
    /// and it leaves [`App::has_pending_query`] set so that the event loop draws
    /// that frame and only then runs the list query. That ordering is the whole
    /// of cache-first rendering: it is structural here, not a promise made in a
    /// comment.
    ///
    /// After [`App::run_pending_query`] the process holds no timer, thread or
    /// subscription: it blocks on terminal input between renders, and the only
    /// thing that can make it query again is a keystroke asking it to.
    pub fn start(environment: &Environment) -> Self {
        let mut app = Self {
            identity: None,
            client: None,
            cache: None,
            pending_list_query: false,
            pending_detail_fetch: None,
            states: IssueStates::default(),
            issue_list: None,
            detail: None,
            cached_detail_updated_at: HashMap::new(),
            filter: String::new(),
            mode: Mode::Normal,
            status: None,
            // Set before anything can fail, so a config warning survives every
            // early return below.
            config_status: (!environment.config_warnings.is_empty())
                .then(|| StatusLine::Config(environment.config_warnings.clone())),
            view: View::List,
            selected: 0,
            detail_scroll: Cell::new(0),
            exit: false,
            list_page_size: environment.list_page_size,
            detail_comment_page_size: environment.detail_comment_page_size,
        };

        let identity = match crate::identity::resolve(environment) {
            Ok(identity) => identity,
            Err(error) => {
                app.status = Some(StatusLine::Identity(error));
                return app;
            }
        };
        app.identity = Some(identity);

        // SPEC §5 step 4: the cache opens before the token is resolved, so that
        // a pane with no token still has something to show. The startup prune
        // that belongs beside this is a later ticket.
        app.cache = Cache::open(environment.state_dir.as_deref());
        app.load_cached_list();

        let Some(token) = environment.token.clone() else {
            // Cached rows stay on screen underneath it: a missing token is a
            // status line, not an empty pane.
            app.status = Some(StatusLine::Api(ApiError::NoToken));
            return app;
        };

        app.client = Some(GithubClient::new(environment.graphql_url.clone(), token));
        app.pending_list_query = true;
        if app.issue_list.is_none() {
            // A cold start has no rows to draw, so the one line it can honestly
            // show is what it is doing. Never a skeleton of rows that may not
            // exist.
            app.status = Some(StatusLine::Fetching);
        }
        app
    }

    /// Whether a query the pane has already drawn for is still to run.
    ///
    /// The event loop asks this after every draw, which is what puts the cached
    /// frame on screen before the request goes out — at startup for the list,
    /// and again whenever a **stale** issue is opened onto its cached body.
    pub fn has_pending_query(&self) -> bool {
        self.pending_list_query || self.pending_detail_fetch.is_some()
    }

    /// Runs whichever query the last frame was drawn ahead of — SPEC §5 step 6's
    /// "then run the list query and re-render", and the same move for one issue.
    ///
    /// Cache-first does not mean network-never: without this a warm pane would
    /// show yesterday's rows with no way to move past them.
    pub fn run_pending_query(&mut self) {
        if self.pending_list_query {
            self.pending_list_query = false;
            self.refresh_list();
            return;
        }
        if let Some(number) = self.pending_detail_fetch.take() {
            self.fetch_detail(number, self.selected);
        }
    }

    /// Puts the cached rows on screen, before anything has been asked of the
    /// network. Silent when there is no cache or nothing cached for this repo.
    fn load_cached_list(&mut self) {
        let (Some(cache), Some(identity)) = (self.cache.as_ref(), self.identity.as_ref()) else {
            return;
        };
        cache.mark_opened(&identity.slug);
        self.issue_list = cache.issue_list(&identity.slug, self.states);
        self.load_cached_detail_ages();
    }

    /// Re-reads the cached details' `updatedAt`s, which is the only thing the
    /// staleness marker is drawn from.
    ///
    /// Called when the pane opens and after every detail written through — the
    /// two moments the answer can change. A list refresh moves the *other* side
    /// of the comparison and needs nothing from here.
    fn load_cached_detail_ages(&mut self) {
        self.cached_detail_updated_at = match (self.cache.as_ref(), self.identity.as_ref()) {
            (Some(cache), Some(identity)) => cache.detail_updated_at(&identity.slug),
            _ => HashMap::new(),
        };
    }

    /// The keys the list view binds: `j`/`k`, the arrows, `g`/`G`, `enter`, `/`,
    /// `esc`, `o`, `r`, `q`. The detail view's own keys are in
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
            // `r` means the thing you are looking at, and in this view that is
            // the list.
            KeyCode::Char('r') => {
                self.refresh_list();
            }
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

    /// The line the pane shows, and the only one it has.
    ///
    /// A config warning is the least urgent thing there is — nothing is broken
    /// and the defaults are already in force — so it yields to a failure or an
    /// empty list and comes back when that clears. Because it is *derived* here
    /// rather than written into `status`, no fetch can overwrite it: it is on
    /// screen for as long as the file it describes is the one that was read.
    pub fn status(&self) -> Option<&StatusLine> {
        self.status.as_ref().or(self.config_status.as_ref())
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

    /// `esc` back, `j`/`k` scroll, `n`/`p` next and previous issue, `r`
    /// re-fetch, `q` close.
    ///
    /// `n` and `p` follow the list's order and stay in the detail view, so a
    /// run of issues reads without a trip back to the list.
    fn handle_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.view = View::List;
                self.detail = None;
                // Leaving takes the queued re-fetch with it: the issue it was
                // for is no longer the thing on screen.
                self.pending_detail_fetch = None;
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
            KeyCode::Char('m') => self.load_more_comments(),
            // `r` means the thing you are looking at, and in this view that is
            // one issue.
            KeyCode::Char('r') => self.refetch_open_issue(),
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

    /// Opens one issue, asking the network for it only when the cache cannot
    /// answer.
    ///
    /// Three cases, and they are the whole of the refresh policy:
    ///
    /// - **Unchanged** — the body was fetched at the `updatedAt` the list row
    ///   still carries, so the cache *is* the answer and **no request is issued
    ///   at all**.
    /// - **Stale** — the two disagree, so the cached issue goes on screen now
    ///   and the re-fetch is queued for after that frame. Cache-first is the
    ///   same rule here as at startup: nothing waits on the network to be
    ///   readable.
    /// - **Never read** — there is nothing to draw ahead of the request, so it
    ///   goes out at once.
    ///
    /// `index` counts the *visible* rows, so `enter` under a filter opens the
    /// issue that is actually under the cursor rather than the nth fetched one.
    fn open(&mut self, index: usize) {
        let Some((number, stale)) = self
            .visible_rows()
            .get(index)
            .map(|row| (row.number, self.is_stale(row)))
        else {
            return;
        };
        match self.cached_detail(number) {
            Some(detail) if !stale => self.show_detail(detail, index),
            Some(detail) => {
                self.show_detail(detail, index);
                self.pending_detail_fetch = Some(number);
            }
            None => self.fetch_detail(number, index),
        }
    }

    /// The cached detail for one issue, or `None` when the cache has never held
    /// it — or when there is no cache at all, which is the viewer outside herdr.
    ///
    /// Not where staleness is decided: the detail this answers with carries the
    /// *list row's* `updatedAt`, because that is the issue's current age and
    /// what the header should say. The `updatedAt` the body was fetched at is
    /// the column [`App::is_stale`] reads, and the two disagreeing is precisely
    /// what stale means.
    fn cached_detail(&self, number: u64) -> Option<IssueDetail> {
        let (cache, identity) = (self.cache.as_ref()?, self.identity.as_ref()?);
        cache.issue_detail(&identity.slug, number)
    }

    /// `m` in the detail view: one more page of comments, appended in order.
    ///
    /// **Exactly one page, and only the page after the one on screen** — the
    /// cursor comes from the last page fetched, so pressing `m` on a
    /// four-hundred-comment thread costs one request each time and never the
    /// remainder. Nothing here is reachable without a keystroke: the affordance
    /// is drawn only while the thread has a page left, and this returns at once
    /// when it does not.
    ///
    /// Immediate rather than queued, for the same reason as
    /// [`App::refetch_open_issue`]: the [`App::has_pending_query`] seam exists
    /// so a cache-first frame can be drawn *before* a request, and the frame
    /// this one would draw — the comments already read — is the frame already
    /// on screen. There is nothing to draw ahead of it.
    ///
    /// A failure is a status line and nothing else. The comments already read
    /// stay exactly where they are, along with the cursor, so `m` can simply be
    /// pressed again.
    fn load_more_comments(&mut self) {
        let Some((number, cursor)) = self.detail.as_ref().and_then(|detail| {
            let cursor = detail.comments_end_cursor.clone()?;
            detail.has_more_comments.then_some((detail.number, cursor))
        }) else {
            return;
        };
        let (Some(client), Some(identity)) = (self.client.as_ref(), self.identity.as_ref()) else {
            return;
        };
        let fetched = client.comment_page(
            &identity.slug,
            number,
            self.detail_comment_page_size,
            &cursor,
        );

        match fetched {
            Ok(page) => {
                if let (Some(cache), Some(identity)) = (self.cache.as_ref(), self.identity.as_ref())
                {
                    // Cached as its own row, with the cursor it ended at, so the
                    // next pane to open this issue reads the thread this far
                    // without asking for any of it again.
                    cache.append_comment_page(&identity.slug, number, &cursor, &page);
                }
                if let Some(detail) = self.detail.as_mut() {
                    detail.comments.extend(page.comments);
                    detail.has_more_comments = page.has_more_comments;
                    detail.comments_end_cursor = page.end_cursor;
                }
                self.status = None;
            }
            Err(error) => self.status = Some(StatusLine::Api(error)),
        }
    }

    /// `r` in the detail view: re-fetch the issue being read.
    ///
    /// Unconditional, because a refresh is the user saying *ask again* and has
    /// no staleness to consult, and immediate, because the issue a cache-first
    /// frame would draw first is the one already on screen.
    ///
    /// **The thread starts again at page one**, however many pages `m` had
    /// walked. That is SPEC §9's invalidation rule — a re-fetched detail drops
    /// its cached comment pages — and it applies to a refresh for the same
    /// reason it applies to a stale issue: the pages already read were asked
    /// for with cursors into the thread as it was, and a thread that has since
    /// been edited cannot be continued from them without risking a comment
    /// shown twice or skipped. The affordance comes back saying what remains,
    /// and `m` walks it again.
    fn refetch_open_issue(&mut self) {
        let Some(number) = self.detail.as_ref().map(|detail| detail.number) else {
            return;
        };
        // Whatever this view was opened owing, it is about to be paid.
        self.pending_detail_fetch = None;
        self.fetch_detail(number, self.selected);
    }

    /// Fetches one issue's detail, writes it through and shows it — the only
    /// thing besides the list query that touches the network, and only ever
    /// because a key asked it to.
    ///
    /// On failure the status line says why and the view does not change: a
    /// failed fetch never clears what is already on screen, whether that is a
    /// cached issue this call was going to replace or the list it was called
    /// from.
    fn fetch_detail(&mut self, number: u64, index: usize) {
        let (Some(client), Some(identity)) = (self.client.as_ref(), self.identity.as_ref()) else {
            return;
        };
        let fetched = client.issue_detail(&identity.slug, number, self.detail_comment_page_size);

        match fetched {
            Ok(detail) => {
                if let (Some(cache), Some(identity)) = (self.cache.as_ref(), self.identity.as_ref())
                {
                    // Which also drops the comment pages cached for this issue
                    // and starts the thread again at page one.
                    cache.save_issue_detail(&identity.slug, &detail);
                }
                self.load_cached_detail_ages();
                self.show_detail(detail, index);
                self.status = None;
            }
            Err(error) => self.status = Some(StatusLine::Api(error)),
        }
    }

    /// Is this row's cached detail behind the list — the `●` of SPEC §11?
    ///
    /// A **disagreement between two `updatedAt`s GitHub issued**, and nothing
    /// else: no clock, no threshold, no TTL. A row whose detail has never been
    /// read is not stale — there is nothing behind the list to mark.
    pub fn is_stale(&self, row: &IssueRow) -> bool {
        self.cached_detail_updated_at
            .get(&row.number)
            .is_some_and(|cached| *cached != row.updated_at)
    }

    /// Puts one issue on screen, wherever it came from.
    fn show_detail(&mut self, detail: IssueDetail, index: usize) {
        self.detail = Some(detail);
        self.detail_scroll.set(0);
        self.selected = index;
        self.view = View::Detail;
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

    /// The only place the issue list is queried — at startup, on `o`, and on
    /// `r` in the list view. Cache-first: a failure becomes a status line and
    /// leaves the rows already on screen exactly as they are.
    ///
    /// Answers whether the list on screen is now the one for [`App::states`].
    fn refresh_list(&mut self) -> bool {
        let (Some(client), Some(identity)) = (self.client.as_ref(), self.identity.as_ref()) else {
            return false;
        };
        let result = client.issue_list(&identity.slug, self.states, self.list_page_size);
        let fetched = match result {
            Ok(list) => {
                // Written through as it arrives, so the next pane on this repo
                // opens on these rows without asking for them.
                if let Some(cache) = self.cache.as_ref() {
                    cache.save_issue_list(&identity.slug, &list);
                }
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
