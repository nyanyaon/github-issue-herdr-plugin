//! The cache, driven the way a pane meets it: a real SQLite database in a temp
//! state directory, written by one app and read by the next.
//!
//! Assertions are on both halves of what the cache is for — what appears on
//! screen, and what a later pane finds in the file. Nothing here mocks below the
//! seam: the database is real, the HTTP client is real, and the only stand-in is
//! the GitHub endpoint.
//!
//! The frame *between* the cached rows and the list query is what most of this
//! file is about, so these tests drive [`App::start`] and
//! [`App::run_pending_query`] separately where the harness's `start` would run
//! both.

mod support;

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::cache::Cache;
use herdr_issues::environment::Environment;
use herdr_issues::github::{IssueComment, IssueDetail, IssueList, IssueRow, IssueStates};
use herdr_issues::identity::Slug;
use serde_json::{Value, json};
use support::{FixtureRepo, StateDir, StubGithub, environment, screen, seconds_ago, start};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// An endpoint with nothing listening on it: the plane, the flaky hotel wifi,
/// the VPN that dropped. Every request to it fails as a transport error.
const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

fn slug() -> Slug {
    Slug::parse(SLUG).expect("a slug this test wrote")
}

fn issue(number: u64, title: &str, label: &str, comments: u64, updated_seconds_ago: i64) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": seconds_ago(updated_seconds_ago),
        "comments": { "totalCount": comments },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": label, "color": "1d76db" }] },
    })
}

fn issue_list_for(name_with_owner: &str, issues: Vec<Value>) -> String {
    json!({
        "data": {
            "repository": {
                "nameWithOwner": name_with_owner,
                "issues": {
                    "totalCount": issues.len(),
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": issues,
                }
            }
        }
    })
    .to_string()
}

fn two_issues() -> Vec<Value> {
    vec![
        // The comment count the list carries is the one the detail view shows:
        // the detail table holds the body, and nothing the list row already has.
        issue(7, "Pane UI shape", "prototype", 107, 18 * 60),
        issue(8, "Packaging and distribution", "grilling", 1, 2 * 3_600),
    ]
}

fn issue_list() -> String {
    issue_list_for(SLUG, two_issues())
}

fn issue_detail() -> String {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "number": 7,
                    "title": "Pane UI shape",
                    "body": "One column, drill-in.",
                    "state": "OPEN",
                    "createdAt": seconds_ago(3 * 86_400),
                    "updatedAt": seconds_ago(18 * 60),
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": 107,
                        "pageInfo": { "hasNextPage": true, "endCursor": "Y3Vyc29yOnYyOpHOAA" },
                        "nodes": [{
                            "author": { "login": "octocat" },
                            "createdAt": seconds_ago(30 * 60),
                            "body": "The first of many.",
                        }],
                    }
                }
            }
        }
    })
    .to_string()
}

/// One stub answering both queries, the way a pane meets GitHub.
fn stub() -> StubGithub {
    StubGithub::routing(vec![
        ("\"number\":7".to_string(), issue_detail()),
        ("issues(first:".to_string(), issue_list()),
    ])
}

/// A pane on this repo, sharing this state directory — which is the one thing
/// every pane has in common.
fn pane(workspace: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        state_dir: Some(state.path.clone()),
        ..environment(workspace, stub)
    }
}

/// The same pane after the network has gone away.
fn offline_pane(workspace: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        graphql_url: UNREACHABLE.to_string(),
        ..pane(workspace, stub, state)
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// The lines that carry something, ignoring the rules that frame the view.
fn content_lines(screen: &str) -> Vec<&str> {
    screen
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('─'))
        .collect()
}

#[test]
fn a_cold_start_with_no_cache_shows_one_status_line_rather_than_a_skeleton() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    let mut app = App::start(&pane(&repo.path, &stub, &state));

    // The frame before the query: nothing is cached, so there is nothing to
    // draw but the one line saying what the pane is doing.
    let cold = screen(&app, 72, 10);
    assert_eq!(
        content_lines(&cold),
        vec![" fetching issues · nothing cached for this repo yet"],
        "a cold start is one status line, not a skeleton of rows:\n{cold}"
    );
    assert_eq!(stub.request_count(), 0);

    app.run_pending_query();

    let warm = screen(&app, 72, 10);
    assert!(warm.contains("Pane UI shape"), "{warm}");
    assert!(
        !warm.contains("fetching issues"),
        "the line goes when the rows arrive:\n{warm}"
    );
    assert_eq!(stub.request_count(), 1);

    // And the rows are in the file, for the next pane to open on.
    let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
    let cached = cache
        .issue_list(&slug(), IssueStates::Open)
        .expect("the list was written through as it arrived");
    assert_eq!(cached.rows.len(), 2);
    assert_eq!(cached.rows[0].title, "Pane UI shape");
    assert_eq!(cached.rows[0].labels, vec!["prototype".to_string()]);
    assert_eq!(cached.rows[1].comment_count, 1);
}

#[test]
fn a_warm_start_renders_the_cached_rows_before_any_request_is_issued() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    // A pane that read this repo earlier, and is gone.
    {
        let stub = stub();
        let first = start(&pane(&repo.path, &stub, &state));
        assert!(screen(&first, 72, 10).contains("Pane UI shape"));
    }

    let stub = stub();
    let mut app = App::start(&pane(&repo.path, &stub, &state));

    // The frame a user sees first, with the network untouched.
    let warm = screen(&app, 72, 10);
    assert_eq!(
        stub.request_count(),
        0,
        "the cached rows are on screen before anything is asked of the network:\n{warm}"
    );
    assert!(warm.contains("Pane UI shape"), "{warm}");
    assert!(warm.contains("Packaging and distribution"), "{warm}");
    assert!(
        warm.contains(&format!("{SLUG} · 2 open · fetched")),
        "the header states the age of what it is showing:\n{warm}"
    );

    // SPEC §5 step 6: having drawn, the pane runs the list query and re-renders.
    app.run_pending_query();
    assert_eq!(stub.request_count(), 1);
    assert!(screen(&app, 72, 10).contains("Pane UI shape"));
}

#[test]
fn a_warm_start_whose_refresh_fails_keeps_the_cached_rows_on_screen() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    {
        let stub = stub();
        start(&pane(&repo.path, &stub, &state));
    }

    // The same pane, reopened with the network gone.
    let stub = stub();
    let app = start(&offline_pane(&repo.path, &stub, &state));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("Pane UI shape"),
        "a failed fetch never clears the cache:\n{screen}"
    );
    assert!(
        screen.contains("offline · could not reach the GitHub API"),
        "the failure is one status line:\n{screen}"
    );
    assert!(
        screen.contains("fetched"),
        "with the age still stated: {screen}"
    );

    // Nor did the failure empty the file.
    let cache = Cache::open(Some(&state.path)).expect("the database is still there");
    assert_eq!(
        cache
            .issue_list(&slug(), IssueStates::Open)
            .expect("the cached rows survived the failed refresh")
            .rows
            .len(),
        2
    );
}

#[test]
fn a_detail_read_in_one_pane_opens_from_the_cache_in_the_next() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    {
        let stub = stub();
        let mut first = start(&pane(&repo.path, &stub, &state));
        press(&mut first, KeyCode::Enter);
        assert!(screen(&first, 72, 16).contains("One column, drill-in."));
        assert_eq!(stub.request_count(), 2, "the list, then the detail");
    }

    // A later pane, with the network gone: everything on screen comes off disk.
    let stub = stub();
    let mut app = start(&offline_pane(&repo.path, &stub, &state));
    press(&mut app, KeyCode::Enter);

    let screen = screen(&app, 72, 16);
    assert!(
        screen.contains("‹ #7 Pane UI shape"),
        "the detail opened from the cache:\n{screen}"
    );
    assert!(screen.contains("One column, drill-in."), "{screen}");
    assert!(
        screen.contains("The first of many."),
        "the cached comment page came with it:\n{screen}"
    );
    assert!(screen.contains("octocat"), "{screen}");
    assert!(
        screen.contains("107 comments"),
        "the counts came from the cached list row:\n{screen}"
    );
    assert!(screen.contains("offline"), "{screen}");
    assert_eq!(stub.request_count(), 0, "nothing was asked of the network");
}

/// The one thing rendering cannot show is a *stale* age: everything a test
/// writes through the app was written a moment ago. So this seeds the real cache
/// with data of a known age and opens a pane on it.
#[test]
fn the_header_states_the_data_age_in_both_the_list_and_the_detail() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let four_hours_ago = herdr_issues::age::now() - 4 * 3_600;
    {
        let cache = Cache::open(Some(&state.path)).expect("open the cache");
        cache.save_issue_list(
            &slug(),
            &IssueList {
                name_with_owner: SLUG.to_string(),
                total_count: 1,
                rows: vec![IssueRow {
                    number: 7,
                    title: "Pane UI shape".to_string(),
                    state: "OPEN".to_string(),
                    updated_at: Some(four_hours_ago),
                    comment_count: 1,
                    author: Some("nyanyaon".to_string()),
                    labels: vec!["prototype".to_string()],
                }],
                fetched_at: four_hours_ago,
            },
        );
        cache.save_issue_detail(
            &slug(),
            &IssueDetail {
                number: 7,
                title: "Pane UI shape".to_string(),
                body: "One column, drill-in.".to_string(),
                state: "OPEN".to_string(),
                updated_at: Some(four_hours_ago),
                author: Some("nyanyaon".to_string()),
                labels: vec!["prototype".to_string()],
                comment_total_count: 1,
                comments: vec![IssueComment {
                    author: Some("octocat".to_string()),
                    created_at: Some(four_hours_ago),
                    body: "The first of many.".to_string(),
                }],
                has_more_comments: false,
                comments_end_cursor: None,
                fetched_at: four_hours_ago,
            },
        );
    }

    let stub = stub();
    let mut app = start(&offline_pane(&repo.path, &stub, &state));

    let list = screen(&app, 72, 10);
    assert!(
        list.contains(&format!("{SLUG} · 1 open · fetched 4h ago")),
        "the list header states the age of what it is showing:\n{list}"
    );

    press(&mut app, KeyCode::Enter);
    let detail = screen(&app, 72, 16);
    assert!(
        detail.contains("fetched 4h ago"),
        "and so does the detail header:\n{detail}"
    );
    assert!(detail.contains("One column, drill-in."), "{detail}");
}

#[test]
fn every_pane_shares_one_database_whoever_has_it_open() {
    let mine = FixtureRepo::with_origin(REMOTE);
    let theirs = FixtureRepo::with_origin("https://github.com/octocat/other-thing.git");
    let state = StateDir::empty();

    // One pane on my repo, left open — which is the state ten panes are in.
    let my_stub = stub();
    let my_pane = start(&pane(&mine.path, &my_stub, &state));
    assert!(screen(&my_pane, 72, 10).contains("Pane UI shape"));

    // A second pane, on another repo, against the same file at the same time.
    let their_stub = StubGithub::serving(issue_list_for(
        "octocat/other-thing",
        vec![issue(3, "Something else entirely", "map", 0, 60)],
    ));
    let their_pane = start(&pane(&theirs.path, &their_stub, &state));
    let theirs_on_screen = screen(&their_pane, 72, 10);
    assert!(theirs_on_screen.contains("Something else entirely"));
    assert!(theirs_on_screen.contains("octocat/other-thing"));

    // Neither repo displaced the other, and the first pane is still readable.
    assert!(screen(&my_pane, 72, 10).contains("Pane UI shape"));
    let third = App::start(&offline_pane(&mine.path, &my_stub, &state));
    assert!(
        screen(&third, 72, 10).contains("Pane UI shape"),
        "a third pane warm-starts on rows the first one wrote"
    );

    assert!(
        state.database().exists(),
        "one database, in the state directory"
    );
}

/// The user-visible face of the migration criterion: reopening a database an
/// earlier pane wrote carries it forward with everything in it. That a *bump*
/// migrates rather than wipes is asserted against a hypothetical next schema
/// step in `cache`'s own tests, since there is only one version so far.
#[test]
fn reopening_the_database_keeps_its_rows_and_its_schema_version() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    let version = {
        let mut first = start(&pane(&repo.path, &stub, &state));
        press(&mut first, KeyCode::Enter);
        Cache::open(Some(&state.path))
            .expect("the database the pane wrote")
            .schema_version()
    };
    assert!(version >= 1, "the schema is versioned");

    let reopened = Cache::open(Some(&state.path)).expect("reopen the database");

    assert_eq!(reopened.schema_version(), version);
    assert_eq!(
        reopened
            .issue_list(&slug(), IssueStates::Open)
            .expect("the list is still there")
            .rows
            .len(),
        2
    );
    assert_eq!(
        reopened
            .issue_detail(&slug(), 7)
            .expect("and so is the detail")
            .body,
        "One column, drill-in."
    );
}
