//! Long threads, driven the way a user meets one: an issue with more comments
//! than a page holds, and the `m` that walks the rest of it.
//!
//! The promise being tested is a count, not a rendering: **opening any issue
//! costs one round trip however long its thread is**, and each `m` costs exactly
//! one more. So every test here counts the requests the stub was sent, and the
//! ones that matter most are the tests that expect *no* request at all.
//!
//! Timestamps are fixed rather than relative, because a re-fetch here turns on
//! two `updatedAt`s disagreeing and must never depend on when the test ran.

mod support;

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::cache::Cache;
use herdr_issues::environment::Environment;
use herdr_issues::identity::Slug;
use serde_json::{Value, json};
use support::{FixtureRepo, StateDir, StubGithub, environment, screen, start};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// #7, the long thread: five comments over three pages.
const SEVEN_AT: &str = "2026-07-20T09:00:00Z";
/// …and the same issue once it has moved on.
const SEVEN_MOVED_AT: &str = "2026-07-27T11:30:00Z";
/// #8, whose whole thread arrives with the issue.
const EIGHT_AT: &str = "2026-07-19T08:00:00Z";

/// The cursor each page of #7 ends at, and the one the page after it is asked
/// for with.
const FIRST_CURSOR: &str = "cursor-1";
const SECOND_CURSOR: &str = "cursor-2";
const LAST_CURSOR: &str = "cursor-3";

/// How many comments #7 has in total — what the affordance counts down from.
const SEVEN_COMMENT_COUNT: u64 = 5;

fn slug() -> Slug {
    Slug::parse(SLUG).expect("a slug this test wrote")
}

/// The title each issue in this repo carries, so no canned response has to
/// repeat one.
fn title(number: u64) -> &'static str {
    match number {
        7 => "Pane UI shape",
        _ => "Packaging and distribution",
    }
}

fn issue(number: u64, comments: u64, updated_at: &str) -> Value {
    json!({
        "number": number,
        "title": title(number),
        "state": "OPEN",
        "updatedAt": updated_at,
        "comments": { "totalCount": comments },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
    })
}

fn issue_list(issues: Vec<Value>) -> String {
    json!({
        "data": {
            "repository": {
                "nameWithOwner": SLUG,
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

fn comment(body: &str) -> Value {
    json!({
        "author": { "login": "octocat" },
        "createdAt": "2026-07-19T12:00:00Z",
        "body": body,
    })
}

/// One issue and one page of its comments, in GitHub's response shape.
///
/// `total_count` is the whole thread's, as GraphQL answers it on every page —
/// the page carries only its own nodes.
fn issue_page(
    number: u64,
    body: &str,
    updated_at: &str,
    total_count: u64,
    comments: Vec<Value>,
    end_cursor: &str,
    has_next: bool,
) -> String {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "number": number,
                    "title": title(number),
                    "body": body,
                    "state": "OPEN",
                    "createdAt": "2026-07-01T09:00:00Z",
                    "updatedAt": updated_at,
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": total_count,
                        "pageInfo": { "hasNextPage": has_next, "endCursor": end_cursor },
                        "nodes": comments,
                    }
                }
            }
        }
    })
    .to_string()
}

/// A repo whose #7 has a thread three pages long, and whose #8 has one that
/// fits.
///
/// The cursor rules come first, because a request for the second page carries
/// `"number":7` as well — what distinguishes it is the `after` it was asked
/// with, which is exactly what the viewer has to get right.
fn stub_with_a_long_thread() -> StubGithub {
    StubGithub::routing(vec![
        (
            format!("\"after\":\"{FIRST_CURSOR}\""),
            issue_page(
                7,
                "One column, drill-in.",
                SEVEN_AT,
                SEVEN_COMMENT_COUNT,
                vec![
                    comment("The third comment."),
                    comment("The fourth comment."),
                ],
                SECOND_CURSOR,
                true,
            ),
        ),
        (
            format!("\"after\":\"{SECOND_CURSOR}\""),
            issue_page(
                7,
                "One column, drill-in.",
                SEVEN_AT,
                SEVEN_COMMENT_COUNT,
                vec![comment("The fifth comment.")],
                LAST_CURSOR,
                false,
            ),
        ),
        (
            "\"number\":7".to_string(),
            issue_page(
                7,
                "One column, drill-in.",
                SEVEN_AT,
                SEVEN_COMMENT_COUNT,
                vec![
                    comment("The first comment."),
                    comment("The second comment."),
                ],
                FIRST_CURSOR,
                true,
            ),
        ),
        (
            "\"number\":8".to_string(),
            issue_page(
                8,
                "Prebuilt binaries, checksum verified.",
                EIGHT_AT,
                1,
                vec![comment("The only comment.")],
                "cursor-eight",
                false,
            ),
        ),
        (
            "issues(first:".to_string(),
            issue_list(vec![
                issue(7, SEVEN_COMMENT_COUNT, SEVEN_AT),
                issue(8, 1, EIGHT_AT),
            ]),
        ),
    ])
}

/// The same repo after #7 has moved on: a shorter thread, and one page of it.
fn stub_after_seven_moved() -> StubGithub {
    StubGithub::routing(vec![
        (
            "\"number\":7".to_string(),
            issue_page(
                7,
                "One column, drill-in. Revised.",
                SEVEN_MOVED_AT,
                1,
                vec![comment("The only comment left.")],
                "cursor-after",
                false,
            ),
        ),
        (
            "issues(first:".to_string(),
            issue_list(vec![issue(7, 1, SEVEN_MOVED_AT), issue(8, 1, EIGHT_AT)]),
        ),
    ])
}

fn pane(workspace: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        state_dir: Some(state.path.clone()),
        ..environment(workspace, stub)
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// What the stub was asked for at `index`, with the whitespace squeezed out, so
/// an assertion reads `"after":"cursor-1"` whether the client sent its JSON
/// compact or pretty-printed.
fn asked_for(stub: &StubGithub, index: usize) -> String {
    stub.request(index)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// The order the comments appear in on screen, by their ordinal.
fn comment_order(screen: &str) -> Vec<&'static str> {
    ["first", "second", "third", "fourth", "fifth"]
        .into_iter()
        .filter(|ordinal| screen.contains(&format!("The {ordinal} comment.")))
        .collect()
}

/// An app on the long-threaded repo, with #7 open on its first page.
fn opened_on_the_long_thread(
    workspace: &Path,
    stub: &StubGithub,
    state: &StateDir,
) -> (App, usize) {
    let mut app = start(&pane(workspace, stub, state));
    press(&mut app, KeyCode::Enter);
    let requests = stub.request_count();
    (app, requests)
}

/// The criterion the whole ticket exists for: a thread of any length opens in
/// one round trip, and says what it is holding back.
#[test]
fn opening_a_long_thread_costs_one_round_trip_and_offers_the_rest() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub_with_a_long_thread();

    let mut app = start(&pane(&repo.path, &stub, &state));
    assert_eq!(stub.request_count(), 1, "the startup list query");

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        stub.request_count(),
        2,
        "opening a five-comment thread costs exactly one detail fetch"
    );
    let opened = screen(&app, 72, 24);
    assert_eq!(
        comment_order(&opened),
        vec!["first", "second"],
        "the first page, and not a comment more:\n{opened}"
    );
    assert!(
        opened.contains("3 more comments · [m]ore"),
        "the rest is offered rather than fetched:\n{opened}"
    );
    assert!(
        opened.contains("m more"),
        "and the key is advertised while it does something:\n{opened}"
    );
}

/// A thread that arrived whole has nothing to offer, and says nothing.
#[test]
fn a_thread_that_fits_in_one_page_shows_no_affordance() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub_with_a_long_thread();

    let mut app = start(&pane(&repo.path, &stub, &state));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);

    let opened = screen(&app, 72, 24);
    assert!(
        opened.contains("‹ #8 Packaging and distribution"),
        "{opened}"
    );
    assert!(opened.contains("The only comment."), "{opened}");
    assert!(
        !opened.contains("[m]ore"),
        "there is nothing behind this thread:\n{opened}"
    );
    assert!(
        !opened.contains("m more"),
        "so the footer does not advertise the key:\n{opened}"
    );

    // …and pressing it anyway asks for nothing.
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(
        stub.request_count(),
        2,
        "the list and the detail, and nothing `m` could add"
    );
}

/// One press, one page, appended in the order it was written — and the cursor
/// the last page ended at is what asked for it.
#[test]
fn m_fetches_exactly_one_page_using_the_stored_cursor_and_appends_it_in_order() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub_with_a_long_thread();
    let (mut app, opened_at) = opened_on_the_long_thread(&repo.path, &stub, &state);
    assert_eq!(opened_at, 2);

    press(&mut app, KeyCode::Char('m'));

    assert_eq!(
        stub.request_count(),
        3,
        "`m` costs exactly one request — one page, never the remainder"
    );
    let asked = asked_for(&stub, 2);
    assert!(
        asked.contains(&format!("\"after\":\"{FIRST_CURSOR}\"")),
        "asked for with the cursor the first page ended at: {asked}"
    );
    assert!(
        asked.contains("\"number\":7"),
        "and for the issue on screen: {asked}"
    );

    let paged = screen(&app, 72, 30);
    assert_eq!(
        comment_order(&paged),
        vec!["first", "second", "third", "fourth"],
        "the new page is appended after the ones already read:\n{paged}"
    );
    assert!(
        paged.contains("1 more comment · [m]ore"),
        "and the count comes down:\n{paged}"
    );

    press(&mut app, KeyCode::Char('m'));

    assert_eq!(stub.request_count(), 4, "the second press, the second page");
    assert!(
        asked_for(&stub, 3).contains(&format!("\"after\":\"{SECOND_CURSOR}\"")),
        "each page is asked for with its predecessor's cursor: {}",
        stub.request(3)
    );
    let whole = screen(&app, 72, 30);
    assert_eq!(
        comment_order(&whole),
        vec!["first", "second", "third", "fourth", "fifth"],
        "the whole thread, in one order:\n{whole}"
    );
    assert!(
        !whole.contains("[m]ore"),
        "reaching the end takes the affordance with it:\n{whole}"
    );
    assert!(
        !whole.contains("m more"),
        "and stops advertising the key:\n{whole}"
    );

    // Pressing it at the end of the thread asks for nothing at all.
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(stub.request_count(), 4, "there is no page left to ask for");
}

/// Each page is cached with the cursor it ended at, so a pane that opens on a
/// half-walked thread resumes it rather than starting it again.
#[test]
fn a_half_walked_thread_is_resumed_from_the_cached_cursor() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();

    // An earlier pane that read two pages and went away.
    {
        let stub = stub_with_a_long_thread();
        let (mut app, _) = opened_on_the_long_thread(&repo.path, &stub, &state);
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(stub.request_count(), 3);
    }

    let stub = stub_with_a_long_thread();
    let mut app = start(&pane(&repo.path, &stub, &state));
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        stub.request_count(),
        1,
        "both cached pages opened without a request between them"
    );
    let reopened = screen(&app, 72, 30);
    assert_eq!(
        comment_order(&reopened),
        vec!["first", "second", "third", "fourth"],
        "in the order they were fetched:\n{reopened}"
    );
    assert!(
        reopened.contains("1 more comment · [m]ore"),
        "with the thread standing exactly where it was left:\n{reopened}"
    );

    press(&mut app, KeyCode::Char('m'));

    assert_eq!(stub.request_count(), 2, "one press, one page");
    assert!(
        asked_for(&stub, 1).contains(&format!("\"after\":\"{SECOND_CURSOR}\"")),
        "continued from the cursor cached with the second page: {}",
        stub.request(1)
    );
    assert_eq!(
        comment_order(&screen(&app, 72, 30)),
        vec!["first", "second", "third", "fourth", "fifth"]
    );
}

/// The other end of the same promise: a thread already walked to its end costs
/// nothing to read again.
#[test]
fn reopening_a_fully_paged_thread_issues_no_request_at_all() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();

    {
        let stub = stub_with_a_long_thread();
        let (mut app, _) = opened_on_the_long_thread(&repo.path, &stub, &state);
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(stub.request_count(), 4, "the list, the issue, two pages");
    }

    // Every page is in the cache, cursor and all.
    {
        let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
        let read = cache.issue_detail(&slug(), 7).expect("the cached detail");
        assert_eq!(
            read.comments.len(),
            5,
            "three pages, read back as one thread"
        );
        assert_eq!(read.comments[0].body, "The first comment.");
        assert_eq!(read.comments[4].body, "The fifth comment.");
        assert!(!read.has_more_comments);
        assert_eq!(read.comments_end_cursor.as_deref(), Some(LAST_CURSOR));
    }

    let stub = stub_with_a_long_thread();
    let mut app = start(&pane(&repo.path, &stub, &state));
    assert_eq!(stub.request_count(), 1, "the startup list query");

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        stub.request_count(),
        1,
        "a thread read to its end costs nothing to read again"
    );
    let reopened = screen(&app, 72, 30);
    assert_eq!(
        comment_order(&reopened),
        vec!["first", "second", "third", "fourth", "fifth"],
        "{reopened}"
    );
    assert!(
        !reopened.contains("[m]ore"),
        "and there is nothing left to offer:\n{reopened}"
    );
}

/// Cache-first cuts both ways: a page that fails to arrive leaves the comments
/// already read exactly where they are.
#[test]
fn a_failed_page_leaves_the_comments_already_read_in_place() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = StubGithub::routing(vec![
        (
            format!("\"after\":\"{FIRST_CURSOR}\""),
            r#"{"errors":[{"message":"the wheels came off"}]}"#.to_string(),
        ),
        (
            "\"number\":7".to_string(),
            issue_page(
                7,
                "One column, drill-in.",
                SEVEN_AT,
                SEVEN_COMMENT_COUNT,
                vec![
                    comment("The first comment."),
                    comment("The second comment."),
                ],
                FIRST_CURSOR,
                true,
            ),
        ),
        (
            "issues(first:".to_string(),
            issue_list(vec![issue(7, SEVEN_COMMENT_COUNT, SEVEN_AT)]),
        ),
    ]);
    let (mut app, _) = opened_on_the_long_thread(&repo.path, &stub, &state);

    press(&mut app, KeyCode::Char('m'));

    let failed = screen(&app, 72, 24);
    assert_eq!(
        comment_order(&failed),
        vec!["first", "second"],
        "a failed page never clears what is on screen:\n{failed}"
    );
    assert!(
        failed.contains("github error · the wheels came off"),
        "it is one status line:\n{failed}"
    );
    assert_eq!(stub.request_count(), 3, "and nothing retried by itself");

    // The cursor survived with them, so the retry is one keystroke.
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(stub.request_count(), 4);
    assert!(asked_for(&stub, 3).contains(&format!("\"after\":\"{FIRST_CURSOR}\"")));
}

/// `r` is a re-fetch, and a re-fetch starts the thread again at page one — the
/// pages walked so far were asked for with cursors into a thread that may have
/// moved (SPEC §9).
#[test]
fn r_restarts_a_walked_thread_at_its_first_page() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub_with_a_long_thread();
    let (mut app, _) = opened_on_the_long_thread(&repo.path, &stub, &state);
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(
        comment_order(&screen(&app, 72, 30)),
        vec!["first", "second", "third", "fourth"]
    );

    press(&mut app, KeyCode::Char('r'));

    assert_eq!(stub.request_count(), 4, "`r` asked for exactly one thing");
    let refreshed = screen(&app, 72, 30);
    assert_eq!(
        comment_order(&refreshed),
        vec!["first", "second"],
        "the thread starts again at its first page:\n{refreshed}"
    );
    assert!(
        refreshed.contains("3 more comments · [m]ore"),
        "and the affordance comes back with what remains:\n{refreshed}"
    );

    let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
    let read = cache.issue_detail(&slug(), 7).expect("the cached detail");
    assert_eq!(
        read.comments.len(),
        2,
        "the pages walked before it went with the body they belonged to"
    );
    assert_eq!(read.comments_end_cursor.as_deref(), Some(FIRST_CURSOR));

    // And walking it again works from the cursor of the page that replaced them.
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(stub.request_count(), 5);
    assert_eq!(
        comment_order(&screen(&app, 72, 30)),
        vec!["first", "second", "third", "fourth"]
    );
}

/// The invariant #16 established, still standing over a thread that was walked
/// to its end: a **stale** issue re-fetched on open drops every page cached for
/// it and starts again at page one.
#[test]
fn a_thread_walked_to_its_end_is_dropped_when_the_issue_goes_stale() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();

    {
        let stub = stub_with_a_long_thread();
        let (mut app, _) = opened_on_the_long_thread(&repo.path, &stub, &state);
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(
            comment_order(&screen(&app, 72, 30)),
            vec!["first", "second", "third", "fourth", "fifth"],
            "the whole thread was read and cached"
        );
    }

    let stub = stub_after_seven_moved();
    let mut app = App::start(&pane(&repo.path, &stub, &state));
    app.run_pending_query();
    assert_eq!(stub.request_count(), 1, "the list query, and nothing else");

    // Cache-first even here: the thread as it was read goes on screen, and the
    // re-fetch is queued behind that frame.
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        comment_order(&screen(&app, 72, 30)),
        vec!["first", "second", "third", "fourth", "fifth"],
        "every cached page is drawn before anything is asked of the network"
    );
    assert_eq!(stub.request_count(), 1);

    app.run_pending_query();

    assert_eq!(
        stub.request_count(),
        2,
        "a stale issue costs one detail fetch, whatever its thread had grown to"
    );
    let re_fetched = screen(&app, 72, 30);
    assert!(
        re_fetched.contains("The only comment left."),
        "{re_fetched}"
    );
    assert!(
        comment_order(&re_fetched).is_empty(),
        "none of the walked thread survived:\n{re_fetched}"
    );
    assert!(
        !re_fetched.contains("[m]ore"),
        "and the new thread fits in its first page:\n{re_fetched}"
    );

    let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
    let read = cache
        .issue_detail(&slug(), 7)
        .expect("the re-fetched detail");
    assert_eq!(read.comments.len(), 1, "one page, and it is the first");
    assert_eq!(read.comments[0].body, "The only comment left.");
    assert!(!read.has_more_comments);
}
