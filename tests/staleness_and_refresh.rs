//! Staleness and refresh, driven the way a user meets them: a pane that read
//! some issues, closed, and opened again onto a repo that moved on.
//!
//! Two things are asserted throughout, and the second is the one that matters:
//! what appears on screen, and **how many requests the stub was sent**. A
//! detail that opens from the cache is only interesting if it opened without
//! asking anyone, so every test here counts.
//!
//! Timestamps are fixed rather than relative, because staleness is a
//! disagreement between two `updatedAt`s and must never depend on when the test
//! ran.

mod support;

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::cache::Cache;
use herdr_issues::environment::Environment;
use herdr_issues::identity::Slug;
use serde_json::{Value, json};
use support::{FixtureRepo, StateDir, StubGithub, environment, screen, settle, start};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// #7, as the earlier pane read it and as it is once it has moved on.
const SEVEN_READ_AT: &str = "2026-07-20T09:00:00Z";
const SEVEN_MOVED_AT: &str = "2026-07-27T11:30:00Z";
/// #8, which has not moved since it was read.
const EIGHT_AT: &str = "2026-07-19T08:00:00Z";
/// #9, which has moved — but was never opened, so nothing is behind it.
const NINE_READ_AT: &str = "2026-07-17T07:00:00Z";
const NINE_MOVED_AT: &str = "2026-07-18T07:00:00Z";

/// The `●` of SPEC §11.
const MARKER: char = '●';

fn slug() -> Slug {
    Slug::parse(SLUG).expect("a slug this test wrote")
}

fn issue(number: u64, title: &str, label: &str, comments: u64, updated_at: &str) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": updated_at,
        "comments": { "totalCount": comments },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": label, "color": "1d76db" }] },
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

fn issue_detail(
    number: u64,
    title: &str,
    body: &str,
    updated_at: &str,
    comments: Vec<Value>,
    end_cursor: &str,
    has_next: bool,
) -> String {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "number": number,
                    "title": title,
                    "body": body,
                    "state": "OPEN",
                    "createdAt": "2026-07-01T09:00:00Z",
                    "updatedAt": updated_at,
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": comments.len(),
                        "pageInfo": { "hasNextPage": has_next, "endCursor": end_cursor },
                        "nodes": comments,
                    }
                }
            }
        }
    })
    .to_string()
}

/// The repo as the earlier pane found it.
fn stub_as_read() -> StubGithub {
    StubGithub::routing(vec![
        (
            "\"number\":7".to_string(),
            issue_detail(
                7,
                "Pane UI shape",
                "The shape as it was.",
                SEVEN_READ_AT,
                vec![comment("Written before it moved on.")],
                "cursor-before",
                true,
            ),
        ),
        (
            "\"number\":8".to_string(),
            issue_detail(
                8,
                "Packaging and distribution",
                "Prebuilt binaries, checksum verified.",
                EIGHT_AT,
                vec![],
                "cursor-eight",
                false,
            ),
        ),
        (
            "issues(first:".to_string(),
            issue_list(vec![
                issue(7, "Pane UI shape", "prototype", 1, SEVEN_READ_AT),
                issue(8, "Packaging and distribution", "grilling", 0, EIGHT_AT),
                issue(9, "Assemble the build spec", "grilling", 0, NINE_READ_AT),
            ]),
        ),
    ])
}

/// The same repo after #7 and #9 have moved on and #8 has not.
fn stub_after_seven_moved() -> StubGithub {
    StubGithub::routing(vec![
        (
            "\"number\":7".to_string(),
            issue_detail(
                7,
                "Pane UI shape",
                "The shape, revised.",
                SEVEN_MOVED_AT,
                vec![
                    comment("Written after it moved on."),
                    comment("And one more."),
                ],
                "cursor-after",
                false,
            ),
        ),
        (
            "\"number\":8".to_string(),
            issue_detail(
                8,
                "Packaging and distribution",
                "Prebuilt binaries, checksum verified.",
                EIGHT_AT,
                vec![],
                "cursor-eight",
                false,
            ),
        ),
        (
            "issues(first:".to_string(),
            issue_list(vec![
                issue(7, "Pane UI shape", "prototype", 2, SEVEN_MOVED_AT),
                issue(8, "Packaging and distribution", "grilling", 0, EIGHT_AT),
                issue(9, "Assemble the build spec", "grilling", 0, NINE_MOVED_AT),
            ]),
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

/// What the stub was asked for at `index`, with the whitespace squeezed out —
/// so an assertion reads `"number":7` whether the client sent its JSON compact
/// or pretty-printed, exactly as the stub's own routing rules do.
fn asked_for(stub: &StubGithub, index: usize) -> String {
    stub.request(index)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{screen}"))
}

/// A pane that read #7 and #8 and is gone — which is what puts a cached detail
/// behind a list row in the first place.
fn read_by_an_earlier_pane(workspace: &Path, state: &StateDir) {
    let stub = stub_as_read();
    let mut first = start(&pane(workspace, &stub, state));
    press(&mut first, KeyCode::Enter);
    assert!(screen(&first, 72, 16).contains("The shape as it was."));
    press(&mut first, KeyCode::Esc);
    press(&mut first, KeyCode::Char('j'));
    press(&mut first, KeyCode::Enter);
    assert!(screen(&first, 72, 16).contains("Prebuilt binaries"));
    assert_eq!(
        stub.request_count(),
        3,
        "the list, then one detail for each issue that was opened"
    );
}

/// The marker is not a clock: it appears exactly when the list disagrees with
/// the cached detail, and only for issues that have a cached detail at all.
#[test]
fn a_row_whose_cached_detail_is_behind_the_list_carries_the_marker() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_by_an_earlier_pane(&repo.path, &state);

    let stub = stub_after_seven_moved();
    let mut app = App::start(&pane(&repo.path, &stub, &state));

    // The warm frame, before the list query: the cached rows agree with the
    // cached details, so nothing is behind anything yet.
    let warm = screen(&app, 72, 10);
    assert_eq!(stub.request_count(), 0);
    assert!(
        !warm.contains(MARKER),
        "nothing is stale against the list it was read with:\n{warm}"
    );

    settle(&mut app);

    let refreshed = screen(&app, 72, 10);
    assert!(
        line_with(&refreshed, "#7").contains(MARKER),
        "#7 moved on since its detail was read:\n{refreshed}"
    );
    assert!(
        !line_with(&refreshed, "#8").contains(MARKER),
        "#8 has not moved, so it carries no marker:\n{refreshed}"
    );
    assert!(
        !line_with(&refreshed, "#9").contains(MARKER),
        "#9 moved on too, but was never read — there is nothing behind it:\n{refreshed}"
    );
    assert_eq!(
        stub.request_count(),
        1,
        "one list query, and no detail query"
    );
}

/// The criterion this whole ticket is for, counted rather than described.
#[test]
fn opening_a_stale_issue_fetches_it_once_and_an_unchanged_one_not_at_all() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_by_an_earlier_pane(&repo.path, &state);

    let stub = stub_after_seven_moved();
    let mut app = App::start(&pane(&repo.path, &stub, &state));
    settle(&mut app);
    assert_eq!(stub.request_count(), 1, "the list query, and nothing else");

    // The stale issue: readable from the cache first, re-fetched after.
    press(&mut app, KeyCode::Enter);
    let cached = screen(&app, 72, 16);
    assert!(
        cached.contains("The shape as it was."),
        "the cached issue is on screen:\n{cached}"
    );
    assert_eq!(
        stub.request_count(),
        1,
        "…and it got there before anything was asked of the network:\n{cached}"
    );
    assert!(
        app.has_pending_query(),
        "with the re-fetch queued behind it"
    );

    app.run_pending_query();

    assert_eq!(
        stub.request_count(),
        2,
        "opening a stale issue costs exactly one detail fetch"
    );
    assert!(
        asked_for(&stub, 1).contains("\"number\":7"),
        "and it asked for the issue that was opened: {}",
        stub.request(1)
    );
    let fresh = screen(&app, 72, 16);
    assert!(fresh.contains("The shape, revised."), "{fresh}");
    assert!(!fresh.contains("The shape as it was."), "{fresh}");
    assert!(!app.has_pending_query(), "nothing is owed now");

    // Having been re-read, the row is no longer behind the list.
    press(&mut app, KeyCode::Esc);
    let list = screen(&app, 72, 10);
    assert!(
        !list.contains(MARKER),
        "the marker goes when the detail catches up:\n{list}"
    );

    // The unchanged issue: no request at all, not even a queued one.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);

    let unchanged = screen(&app, 72, 16);
    assert!(
        unchanged.contains("‹ #8 Packaging and distribution"),
        "{unchanged}"
    );
    assert!(
        unchanged.contains("Prebuilt binaries, checksum verified."),
        "it opened from the cache:\n{unchanged}"
    );
    assert!(
        !app.has_pending_query(),
        "an unchanged issue queues nothing"
    );
    assert_eq!(
        stub.request_count(),
        2,
        "opening an unchanged issue issues no request whatsoever"
    );
}

/// A re-fetched issue starts its thread again: the pages cached for it go with
/// the body they belonged to, cursor and all.
#[test]
fn re_fetching_a_stale_issue_drops_its_cached_comment_pages() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_by_an_earlier_pane(&repo.path, &state);

    // What the earlier pane left behind: one page, unfinished.
    {
        let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
        let read = cache.issue_detail(&slug(), 7).expect("the cached detail");
        assert_eq!(read.comments.len(), 1);
        assert_eq!(read.comments[0].body, "Written before it moved on.");
        assert!(read.has_more_comments);
        assert_eq!(read.comments_end_cursor.as_deref(), Some("cursor-before"));
    }

    let stub = stub_after_seven_moved();
    let mut app = App::start(&pane(&repo.path, &stub, &state));
    settle(&mut app);
    press(&mut app, KeyCode::Enter);
    app.run_pending_query();

    let thread = screen(&app, 72, 24);
    assert!(thread.contains("Written after it moved on."), "{thread}");
    assert!(thread.contains("And one more."), "{thread}");
    assert!(
        !thread.contains("Written before it moved on."),
        "the old page is gone rather than appended to:\n{thread}"
    );

    let cache = Cache::open(Some(&state.path)).expect("the database the pane wrote");
    let read = cache
        .issue_detail(&slug(), 7)
        .expect("the re-fetched detail");
    assert_eq!(read.body, "The shape, revised.");
    assert_eq!(read.comments.len(), 2, "the first page of the new thread");
    assert!(
        read.comments
            .iter()
            .all(|comment| comment.body != "Written before it moved on."),
        "nothing of the old thread survived"
    );
    assert_eq!(read.comments_end_cursor.as_deref(), Some("cursor-after"));
    assert!(!read.has_more_comments, "and the cursor came with it");
}

/// `r` means the thing you are looking at. In the list view that is the list.
#[test]
fn r_refreshes_the_list_in_the_list_view() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub_as_read();
    let mut app = start(&environment(&repo.path, &stub));
    assert_eq!(stub.request_count(), 1, "the startup list query");

    press(&mut app, KeyCode::Char('r'));

    assert_eq!(stub.request_count(), 2, "r asked for the list again");
    assert!(
        asked_for(&stub, 1).contains("issues(first:"),
        "and it was the list query it asked for: {}",
        stub.request(1)
    );
    let refreshed = screen(&app, 72, 10);
    assert!(refreshed.contains("Pane UI shape"), "{refreshed}");
    assert!(
        refreshed.contains("r refresh"),
        "and the key is advertised:\n{refreshed}"
    );
}

/// …and in the detail view it is that issue, however fresh the cache thinks it
/// is.
#[test]
fn r_refetches_the_open_issue_in_the_detail_view() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_by_an_earlier_pane(&repo.path, &state);

    // A pane where nothing is stale, so nothing would be fetched on its own.
    let stub = stub_as_read();
    let mut app = App::start(&pane(&repo.path, &stub, &state));
    settle(&mut app);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(stub.request_count(), 1, "#8 opened from the cache");
    let opened = screen(&app, 72, 16);
    assert!(
        opened.contains("‹ #8 Packaging and distribution"),
        "{opened}"
    );
    assert!(
        opened.contains("r refresh"),
        "the key is advertised here too:\n{opened}"
    );

    press(&mut app, KeyCode::Char('r'));

    assert_eq!(stub.request_count(), 2, "r asked for exactly one thing");
    assert!(
        asked_for(&stub, 1).contains("\"number\":8"),
        "and it was the issue on screen, not the list: {}",
        stub.request(1)
    );
    assert!(
        !app.has_pending_query(),
        "a refresh is immediate — its frame is already on screen"
    );
    let refreshed = screen(&app, 72, 16);
    assert!(
        refreshed.contains("Prebuilt binaries, checksum verified."),
        "{refreshed}"
    );
    assert!(
        refreshed.contains("‹ #8 Packaging and distribution"),
        "still the same issue:\n{refreshed}"
    );
}

/// A refresh that fails is a status line and nothing more — the issue being
/// read stays exactly where it was.
#[test]
fn a_failed_refresh_leaves_the_issue_on_screen() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    read_by_an_earlier_pane(&repo.path, &state);

    let stub = stub_as_read();
    let mut app = App::start(&Environment {
        // Reachable for as long as the pane needs the cache, and gone by the
        // time it asks for anything.
        graphql_url: "http://127.0.0.1:9/graphql".to_string(),
        ..pane(&repo.path, &stub, &state)
    });
    settle(&mut app);
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 72, 16).contains("The shape as it was."));

    press(&mut app, KeyCode::Char('r'));

    let after = screen(&app, 72, 16);
    assert!(
        after.contains("The shape as it was."),
        "a failed refresh never clears what is on screen:\n{after}"
    );
    assert!(
        after.contains("offline · showing cache from just now"),
        "it is one status line, over the cached issue it left on screen:\n{after}"
    );
    assert_eq!(stub.request_count(), 0, "nothing reached the stub at all");
}
