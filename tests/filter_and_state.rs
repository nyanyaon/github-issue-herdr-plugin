//! Finding an issue: the filter and the state toggle, driven by key events and
//! asserted on the rendered screen and on what the stub endpoint was asked.
//!
//! The filter's whole promise is that it costs nothing, so several of these
//! tests assert on the *number* of requests rather than on their content.

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use serde_json::json;
use support::{FixtureRepo, StubGithub, environment, screen, seconds_ago};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// A canned answer to the issue list query, in GitHub's response shape.
fn issue_list(issues: Vec<serde_json::Value>) -> String {
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

fn issue(number: u64, title: &str, label: &str, author: &str) -> serde_json::Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": seconds_ago(18 * 60),
        "comments": { "totalCount": 0 },
        "author": { "login": author },
        "labels": { "nodes": [{ "name": label, "color": "1d76db" }] },
    })
}

/// Three issues that differ in every field the filter matches on, so a hit can
/// only have come from the field the test meant.
fn three_issues() -> Vec<serde_json::Value> {
    vec![
        issue(7, "Pane UI shape", "prototype", "nyanyaon"),
        issue(
            42,
            "Walking skeleton: issue list in a pane",
            "map",
            "octocat",
        ),
        issue(11, "Token discovery", "grilling", "hubot"),
    ]
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

/// Which issue numbers are drawn, in order.
fn numbers_on_screen(screen: &str) -> Vec<u64> {
    screen
        .lines()
        .filter_map(|line| line.split_whitespace().find(|word| word.starts_with('#')))
        .filter_map(|word| word.trim_start_matches('#').parse().ok())
        .collect()
}

fn selected_line(screen: &str) -> &str {
    screen
        .lines()
        .find(|line| line.contains('▸'))
        .unwrap_or_else(|| panic!("nothing selected in:\n{screen}"))
}

/// The `$states` variable of the request the stub recorded at `index`.
fn states_argument(stub: &StubGithub, index: usize) -> serde_json::Value {
    let body: serde_json::Value =
        serde_json::from_str(&stub.request(index)).expect("a JSON request body");
    body["variables"]["states"].clone()
}

#[test]
fn slash_filters_on_title_number_label_and_author_at_once() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));

    // One query per field, each from a fresh pane, so no filter leaks into the
    // next.
    for (query, expected) in [
        ("walking", 42),  // title
        ("42", 42),       // number
        ("#11", 11),      // number, typed the way it is displayed
        ("grilling", 11), // label
        ("octocat", 42),  // author
        ("PROTOTYPE", 7), // case-insensitive
        ("pane ui", 7),   // a fragment spanning two words
    ] {
        let mut app = App::start(&environment(&repo.path, &stub));
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, query);

        let screen = screen(&app, 72, 10);
        assert_eq!(
            numbers_on_screen(&screen),
            vec![expected],
            "{query:?} should match only #{expected} in:\n{screen}"
        );
    }
}

#[test]
fn the_header_counts_the_filtered_rows_against_the_total() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    let full = screen(&app, 72, 10);
    assert!(full.contains(&format!(" {SLUG} · 3 open · ")), "{full}");

    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "o");
    let filtered = screen(&app, 72, 10);
    // "Token discovery"/hubot, "prototype" and "octocat" all carry an `o`.
    assert!(filtered.contains("· 3 of 3 shown ·"), "{filtered}");
    // What has been typed is on screen while it is being typed.
    assert!(filtered.contains(" /o"), "{filtered}");

    type_text(&mut app, "cto");
    let narrower = screen(&app, 72, 10);
    assert!(narrower.contains("· 1 of 3 shown ·"), "{narrower}");
    assert_eq!(numbers_on_screen(&narrower), vec![42]);

    press(&mut app, KeyCode::Backspace);
    let widened = screen(&app, 72, 10);
    assert!(widened.contains("· 1 of 3 shown ·"), "{widened}");
    assert!(widened.contains(" /oct"), "{widened}");
}

#[test]
fn esc_clears_the_filter_and_restores_the_full_list() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "walking");
    assert_eq!(numbers_on_screen(&screen(&app, 72, 10)), vec![42]);

    press(&mut app, KeyCode::Esc);

    let restored = screen(&app, 72, 10);
    assert_eq!(numbers_on_screen(&restored), vec![7, 42, 11]);
    assert!(restored.contains(" · 3 open · "), "{restored}");
    // Out of typing mode, so the key hints are back and letters are commands.
    assert!(restored.contains("/ filter"), "{restored}");
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_exit());
}

#[test]
fn filtering_issues_no_request_at_any_keystroke() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    // The one query a cold start makes.
    assert_eq!(stub.request_count(), 1);

    press(&mut app, KeyCode::Char('/'));
    // Including the letters that are commands outside typing mode.
    type_text(&mut app, "token or #42 rq");
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);

    assert_eq!(
        stub.request_count(),
        1,
        "the filter runs on cached rows and never asks the API"
    );
}

#[test]
fn typing_mode_makes_letters_text_rather_than_commands() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "q");

    assert!(
        !app.should_exit(),
        "`q` typed into the filter is not `quit`"
    );
    let screen = screen(&app, 72, 10);
    assert!(screen.contains(" /q"), "{screen}");
    // `q` matches nothing, so the list is empty and the count says so.
    assert!(screen.contains("· 0 of 3 shown ·"), "{screen}");
    assert!(numbers_on_screen(&screen).is_empty(), "{screen}");

    // `enter` hands the plain keys back to the list, keeping the filter.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_exit());
}

#[test]
fn the_selection_lands_on_a_row_that_still_exists() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    // The last row of three.
    press(&mut app, KeyCode::Char('G'));
    assert!(selected_line(&screen(&app, 72, 10)).contains("#11"));

    // A filter that leaves one row: the selection moves onto it.
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "walking");
    let narrowed = screen(&app, 72, 10);
    assert_eq!(numbers_on_screen(&narrowed), vec![42]);
    assert!(selected_line(&narrowed).contains("#42"), "{narrowed}");

    // A filter that leaves none: nothing is selected, and nothing panics.
    type_text(&mut app, "zzz");
    let empty = screen(&app, 72, 10);
    assert!(numbers_on_screen(&empty).is_empty(), "{empty}");
    assert!(!empty.contains('▸'), "{empty}");

    // Clearing it puts the selection back at the top of the restored list.
    press(&mut app, KeyCode::Esc);
    let restored = screen(&app, 72, 10);
    assert_eq!(numbers_on_screen(&restored), vec![7, 42, 11]);
    assert!(selected_line(&restored).contains("#7"), "{restored}");
}

#[test]
fn o_cycles_open_closed_all_and_queries_the_state_it_moved_to() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));

    // The cold start asks for open issues.
    assert_eq!(stub.request_count(), 1);
    assert_eq!(states_argument(&stub, 0), json!(["OPEN"]));
    assert!(screen(&app, 72, 10).contains(" · 3 open · "));

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(stub.request_count(), 2, "one query per press, not more");
    assert_eq!(states_argument(&stub, 1), json!(["CLOSED"]));
    assert!(screen(&app, 72, 10).contains(" · 3 closed · "));

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(stub.request_count(), 3);
    // Every state is the absence of the argument — `IssueState` has no `ALL`.
    assert_eq!(states_argument(&stub, 2), json!(null));
    assert!(screen(&app, 72, 10).contains(" · 3 issues · "));

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(stub.request_count(), 4);
    assert_eq!(states_argument(&stub, 3), json!(["OPEN"]));
    assert!(screen(&app, 72, 10).contains(" · 3 open · "));
}

#[test]
fn a_state_with_no_issues_says_which_state_and_how_to_leave_it() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = StubGithub::serving(issue_list(vec![]));
    let mut app = App::start(&environment(&repo.path, &stub));

    assert!(
        screen(&app, 72, 10).contains("no open issues · [o] to include closed"),
        "{}",
        screen(&app, 72, 10)
    );

    press(&mut app, KeyCode::Char('o'));
    assert!(
        screen(&app, 72, 10).contains("no closed issues · [o] to include all"),
        "{}",
        screen(&app, 72, 10)
    );

    press(&mut app, KeyCode::Char('o'));
    assert!(
        screen(&app, 72, 10).contains("no issues"),
        "{}",
        screen(&app, 72, 10)
    );
}

#[test]
fn a_failed_toggle_leaves_the_rows_already_on_screen() {
    let repo = FixtureRepo::with_origin(REMOTE);
    // The endpoint answers the cold start and then stops working.
    let stub = StubGithub::serving_once(issue_list(three_issues()));
    let mut app = App::start(&environment(&repo.path, &stub));
    assert_eq!(numbers_on_screen(&screen(&app, 72, 10)), vec![7, 42, 11]);

    press(&mut app, KeyCode::Char('o'));

    let after = screen(&app, 72, 10);
    assert_eq!(
        numbers_on_screen(&after),
        vec![7, 42, 11],
        "a failed fetch never clears what is already shown:\n{after}"
    );
    assert!(after.contains("github error"), "{after}");
    // And the header still describes the rows that are actually on screen.
    assert!(after.contains(" · 3 open · "), "{after}");
}
