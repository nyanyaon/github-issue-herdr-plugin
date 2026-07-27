//! Every way the API can fail, driven from the stub server against a real
//! SQLite cache in a temp directory.
//!
//! Three things are asserted of each failure, because ADR-0005 promises all
//! three: the one status line it puts up, the cached rows it leaves untouched
//! underneath — a failure is never an empty pane — and the retry it does not
//! make. Nothing here mocks below the seam; the database is real, the HTTP
//! client is real, and the only stand-in is the GitHub endpoint.
//!
//! The cache is seeded directly, with a known age, because the one thing a test
//! that fetches its own data cannot show is a *stale* age: everything written
//! through the app was written a moment ago, and the offline line has to state
//! four hours.

mod support;

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::cache::Cache;
use herdr_issues::environment::Environment;
use herdr_issues::github::{IssueComment, IssueDetail, IssueList, IssueRow, IssueStates};
use herdr_issues::identity::Slug;
use serde_json::json;
use support::{FixtureRepo, StateDir, StubGithub, StubResponse, environment, screen, start};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// An endpoint with nothing listening on it: the plane, the hotel wifi, the VPN
/// that dropped. Every request to it fails as a transport error.
const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

/// How old everything the earlier pane cached is, so the offline line has an age
/// to state that no test wrote a moment ago.
const FOUR_HOURS: i64 = 4 * 3_600;

fn slug() -> Slug {
    Slug::parse(SLUG).expect("a slug this test wrote")
}

/// Two rows and one issue body, cached four hours ago by a pane that is gone.
///
/// This is what every failure below is asked to leave alone.
fn read_four_hours_ago(state: &StateDir) {
    let then = herdr_issues::age::now() - FOUR_HOURS;
    let cache = Cache::open(Some(&state.path)).expect("open the cache");
    cache.save_issue_list(
        &slug(),
        &IssueList {
            name_with_owner: SLUG.to_string(),
            total_count: 2,
            rows: vec![
                IssueRow {
                    number: 7,
                    title: "Pane UI shape".to_string(),
                    state: "OPEN".to_string(),
                    updated_at: Some(then),
                    comment_count: 1,
                    author: Some("nyanyaon".to_string()),
                    labels: vec!["prototype".to_string()],
                },
                IssueRow {
                    number: 8,
                    title: "Packaging and distribution".to_string(),
                    state: "OPEN".to_string(),
                    updated_at: Some(then),
                    comment_count: 0,
                    author: Some("nyanyaon".to_string()),
                    labels: vec!["grilling".to_string()],
                },
            ],
            fetched_at: then,
        },
    );
    cache.save_issue_detail(
        &slug(),
        &IssueDetail {
            number: 7,
            title: "Pane UI shape".to_string(),
            body: "One column, drill-in.".to_string(),
            state: "OPEN".to_string(),
            updated_at: Some(then),
            author: Some("nyanyaon".to_string()),
            labels: vec!["prototype".to_string()],
            comment_total_count: 1,
            comments: vec![IssueComment {
                author: Some("octocat".to_string()),
                created_at: Some(then),
                body: "The first of many.".to_string(),
            }],
            has_more_comments: false,
            comments_end_cursor: None,
            fetched_at: then,
        },
    );
}

/// A pane on that repo, sharing that cache, talking to this stub.
fn pane(repo: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        state_dir: Some(state.path.clone()),
        ..environment(repo, stub)
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// A successful list response, for the stub that answers after a failure.
fn issue_list() -> String {
    json!({
        "data": {
            "repository": {
                "nameWithOwner": SLUG,
                "issues": {
                    "totalCount": 1,
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": [{
                        "number": 7,
                        "title": "Pane UI shape",
                        "state": "OPEN",
                        "updatedAt": herdr_issues::age::format_timestamp(herdr_issues::age::now()),
                        "comments": { "totalCount": 1 },
                        "author": { "login": "nyanyaon" },
                        "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    }],
                }
            }
        }
    })
    .to_string()
}

/// The `NOT_FOUND` GitHub answers with for a repo that is missing *and* for one
/// this token cannot see. It cannot tell them apart, so neither can the line.
fn not_found() -> String {
    json!({
        "data": { "repository": null },
        "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository" }]
    })
    .to_string()
}

/// What every failure leaves behind: both cached rows, and the header still
/// stating their age.
fn assert_cached_rows_survived(screen: &str, state: &StateDir) {
    assert!(
        screen.contains("Pane UI shape") && screen.contains("Packaging and distribution"),
        "a failure is a status line, not an empty pane:\n{screen}"
    );
    assert!(
        screen.contains(&format!("{SLUG} · 2 open · fetched 4h ago")),
        "with the header still stating the age of what it is showing:\n{screen}"
    );
    let cache = Cache::open(Some(&state.path)).expect("the database is still there");
    assert_eq!(
        cache
            .issue_list(&slug(), IssueStates::Open)
            .expect("the cached rows survived the failure")
            .rows
            .len(),
        2,
        "nor did the failure empty the file"
    );
}

#[test]
fn a_rejected_token_says_so_over_the_rows_it_left_alone() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::responding(401, r#"{"message":"Bad credentials"}"#);

    let app = start(&pane(&repo.path, &stub, &state));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("token rejected · check GITHUB_TOKEN or run `gh auth login`"),
        "{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
}

#[test]
fn a_repo_the_token_cannot_see_reads_the_same_as_one_that_is_missing() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::serving(not_found());

    let app = start(&pane(&repo.path, &stub, &state));

    // Wide enough for the whole line: the status line truncates to the pane,
    // like every other line here.
    let screen = screen(&app, 100, 10);
    assert!(
        screen.contains(&format!("{SLUG} not found — or your token can't see it")),
        "one line covers both readings, because the API gives us no way to tell \
         them apart:\n{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
}

#[test]
fn a_rate_limited_refresh_states_when_it_resets_and_names_the_retry() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    // What GitHub sends when the hourly points are gone: the absolute second the
    // window rolls over.
    let stub = StubGithub::responding_with(
        StubResponse::status(403, r#"{"message":"API rate limit exceeded"}"#)
            .with_header("x-ratelimit-remaining", 0)
            .with_header("x-ratelimit-reset", herdr_issues::age::now() + 12 * 60),
    );

    let app = start(&pane(&repo.path, &stub, &state));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("rate limited · resets in 12m · [r] retry"),
        "the reset is on screen, and so is the only thing that retries:\n{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
}

#[test]
fn a_secondary_limit_is_read_off_retry_after() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    // The secondary limit says how long to wait, not when the window rolls.
    let stub = StubGithub::responding_with(
        StubResponse::status(
            403,
            r#"{"message":"You have exceeded a secondary rate limit"}"#,
        )
        .with_header("retry-after", 90),
    );

    let app = start(&pane(&repo.path, &stub, &state));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("rate limited · resets in 2m · [r] retry"),
        "{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
}

#[test]
fn a_403_carrying_no_rate_limit_headers_is_not_reported_as_rate_limiting() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::responding(403, r#"{"message":"Resource not accessible"}"#);

    let app = start(&pane(&repo.path, &stub, &state));

    let screen = screen(&app, 72, 10);
    assert!(
        !screen.contains("rate limited"),
        "there is no clock to wait out, so the line must not name one:\n{screen}"
    );
    assert!(screen.contains("github error · HTTP 403"), "{screen}");
    assert_cached_rows_survived(&screen, &state);
}

#[test]
fn a_transport_failure_states_the_age_of_the_cache_it_left_on_screen() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::serving(issue_list());

    let mut app = start(&Environment {
        graphql_url: UNREACHABLE.to_string(),
        ..pane(&repo.path, &stub, &state)
    });

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("offline · showing cache from 4h ago"),
        "SPEC §11's line, with the age of what a plane leaves you reading:\n{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
    assert_eq!(stub.request_count(), 0, "nothing reached a server at all");

    // Nor does a dead network turn into background work: the pane asks for
    // nothing until a key says so. The narrower question again — a queued
    // startup prune is local housekeeping, not a request.
    for _ in 0..3 {
        assert!(!app.has_pending_request());
        app.run_pending_query();
    }
    assert_eq!(stub.request_count(), 0);
}

#[test]
fn a_cold_start_with_no_cache_has_no_age_to_state_and_says_so() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = StubGithub::serving(issue_list());

    let app = start(&Environment {
        graphql_url: UNREACHABLE.to_string(),
        ..pane(&repo.path, &stub, &state)
    });

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("offline · could not reach the GitHub API"),
        "with nothing cached, `showing cache from` would be a lie:\n{screen}"
    );
    assert!(!screen.contains("showing cache"), "{screen}");
}

#[test]
fn with_no_token_the_line_names_all_three_ways_over_the_cached_rows() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::serving(issue_list());

    let app = start(&Environment {
        token: None,
        ..pane(&repo.path, &stub, &state)
    });

    let screen = screen(&app, 100, 10);
    assert!(
        screen.contains(
            "no GitHub token found · set GITHUB_TOKEN, run `gh auth login`, or set token_file"
        ),
        "{screen}"
    );
    assert_cached_rows_survived(&screen, &state);
    assert_eq!(
        stub.request_count(),
        0,
        "a pane with no token asks for nothing"
    );
}

/// The failure a user is most likely to be *inside* when it happens: reading an
/// issue, and pressing `r`.
#[test]
fn a_failure_in_the_detail_view_leaves_the_issue_and_the_rows_behind_it() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::responding(401, r#"{"message":"Bad credentials"}"#);

    let mut app = App::start(&pane(&repo.path, &stub, &state));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 72, 16).contains("One column, drill-in."));

    press(&mut app, KeyCode::Char('r'));

    let detail = screen(&app, 72, 16);
    assert!(
        detail.contains("One column, drill-in."),
        "the issue being read stays on screen:\n{detail}"
    );
    assert!(detail.contains("token rejected"), "{detail}");

    // And so does the list it was opened from.
    press(&mut app, KeyCode::Esc);
    assert_cached_rows_survived(&screen(&app, 72, 10), &state);
}

/// ADR-0005: nothing retries by itself. `r` is the retry, and a pane that has
/// failed is a pane blocking on input.
#[test]
fn no_failure_ever_queues_a_retry() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let failures = [
        StubResponse::status(401, r#"{"message":"Bad credentials"}"#),
        StubResponse::ok(not_found()),
        StubResponse::status(403, "{}")
            .with_header("x-ratelimit-reset", herdr_issues::age::now() + 12 * 60),
        StubResponse::status(500, "{}"),
    ];

    for failure in failures {
        let state = StateDir::empty();
        read_four_hours_ago(&state);
        let stub = StubGithub::responding_with(failure.clone());

        let mut app = App::start(&pane(&repo.path, &stub, &state));
        app.run_pending_query();
        assert_eq!(
            stub.request_count(),
            1,
            "the one query the pane was opened for"
        );

        // Everything the event loop does between keystrokes, several times over:
        // it draws, and it asks whether anything is owed. No *request* is —
        // the startup prune may still be queued behind this, which is work but
        // is not a retry, so this asks the narrower question deliberately.
        for _ in 0..3 {
            assert!(
                !app.has_pending_request(),
                "a failure queues no retry: {failure:?}"
            );
            app.run_pending_query();
            screen(&app, 72, 10);
        }

        assert_eq!(
            stub.request_count(),
            1,
            "and nothing went out that a key did not ask for: {failure:?}"
        );
    }
}

/// ADR-0005: remaining quota is displayed **only** when a request is actually
/// rate-limited. A successful response carries the same headers, and none of it
/// reaches the screen.
#[test]
fn quota_is_never_on_screen_when_the_request_succeeded() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::responding_with(
        StubResponse::ok(issue_list())
            .with_header("x-ratelimit-limit", 5_000)
            .with_header("x-ratelimit-remaining", 4_999)
            .with_header("x-ratelimit-used", 1)
            .with_header("x-ratelimit-reset", herdr_issues::age::now() + 12 * 60),
    );

    let mut app = start(&pane(&repo.path, &stub, &state));
    let list = screen(&app, 72, 10);
    press(&mut app, KeyCode::Enter);
    let detail = screen(&app, 72, 16);

    for screen in [&list, &detail] {
        for noise in [
            "rate",
            "limit",
            "quota",
            "remaining",
            "4999",
            "4,999",
            "5000",
        ] {
            assert!(
                !screen.to_lowercase().contains(noise),
                "a quota is noise about a limit manual refresh cannot reach, and \
                 nothing but a rate-limited request may put it on screen — found \
                 {noise:?} in:\n{screen}"
            );
        }
    }
}

/// And when the rate limit does lift, the line goes with it: the quota is on
/// screen for exactly as long as the failure is.
#[test]
fn the_rate_limit_line_goes_when_a_retry_gets_through() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_four_hours_ago(&state);
    let stub = StubGithub::responding_then(
        StubResponse::status(403, "{}")
            .with_header("x-ratelimit-remaining", 0)
            .with_header("x-ratelimit-reset", herdr_issues::age::now() + 12 * 60),
        StubResponse::ok(issue_list()),
    );

    let mut app = start(&pane(&repo.path, &stub, &state));
    assert!(screen(&app, 72, 10).contains("rate limited"));

    // `r` is the retry, and the only thing that ever was.
    press(&mut app, KeyCode::Char('r'));

    let screen = screen(&app, 72, 10);
    assert!(
        !screen.contains("rate limited"),
        "the line lasts exactly as long as the failure:\n{screen}"
    );
    assert!(screen.contains("Pane UI shape"), "{screen}");
    assert_eq!(
        stub.request_count(),
        2,
        "one query, then the one `r` asked for"
    );
}
