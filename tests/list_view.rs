//! The list view, driven the way a user drives it: an app built from an
//! environment description, key events in, rendered text out.
//!
//! Every assertion here is on what appears on screen.

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use serde_json::json;
use support::{FixtureRepo, StubGithub, environment, screen, seconds_ago};

/// A canned answer to the issue list query, in GitHub's response shape.
fn issue_list(name_with_owner: &str, issues: Vec<serde_json::Value>) -> String {
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

fn issue(
    number: u64,
    title: &str,
    labels: &[&str],
    comment_count: u64,
    updated_seconds_ago: i64,
) -> serde_json::Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": seconds_ago(updated_seconds_ago),
        "comments": { "totalCount": comment_count },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": labels.iter().map(|name| json!({ "name": name, "color": "1d76db" })).collect::<Vec<_>>() },
    })
}

/// Three issues, most recently updated first, as the query orders them.
fn three_issues() -> Vec<serde_json::Value> {
    vec![
        issue(7, "Pane UI shape", &["prototype"], 0, 18 * 60),
        issue(
            42,
            "Walking skeleton: issue list in a pane",
            &["map", "spec"],
            3,
            2 * 3_600,
        ),
        issue(11, "Token discovery", &["grilling"], 12, 3 * 86_400),
    ]
}

fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{screen}"))
}

fn selected_line(screen: &str) -> &str {
    line_with(screen, "▸")
}

#[test]
fn cold_start_renders_a_row_for_each_issue() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list(
        "nyanyaon/github-issue-herdr-plugin",
        three_issues(),
    ));
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);

    assert!(
        screen.contains(" nyanyaon/github-issue-herdr-plugin · 3 open · fetched just now"),
        "header missing in:\n{screen}"
    );

    // Number, title, first label, comment count, relative age.
    let row = line_with(&screen, "#42");
    assert!(
        row.contains("Walking skeleton: issue list in a pane"),
        "{row}"
    );
    assert!(row.contains("map"), "{row}");
    assert!(!row.contains("spec"), "only the first label fits: {row}");
    assert!(row.contains("3 ·"), "{row}");
    assert!(row.ends_with("2h"), "{row}");

    let older = line_with(&screen, "#11");
    assert!(older.contains("12 ·"), "{older}");
    assert!(older.ends_with("3d"), "{older}");

    // Ordered by most recently updated.
    assert!(
        screen.find("#7") < screen.find("#42") && screen.find("#42") < screen.find("#11"),
        "rows out of order in:\n{screen}"
    );

    // The keys this slice binds, and only those.
    assert!(
        screen.contains("j/k move   enter open   g/G top/bottom   q close"),
        "{screen}"
    );
}

#[test]
fn a_row_with_no_comments_shows_no_count() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list(
        "nyanyaon/github-issue-herdr-plugin",
        three_issues(),
    ));
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);
    let row = line_with(&screen, "#7");

    assert!(row.contains("Pane UI shape"), "{row}");
    assert!(row.contains("prototype"), "{row}");
    assert!(row.ends_with("18m"), "{row}");
    assert!(
        !row.contains("0 ·"),
        "a zero comment count is not drawn: {row}"
    );
}

#[test]
fn the_header_names_the_repo_the_api_returned_not_the_remote() {
    // The remote still points at the name the repo had before it was renamed.
    let repo = FixtureRepo::with_origin("https://github.com/octocat/old-name.git");
    let stub = StubGithub::serving(issue_list(
        "octocat/new-name",
        vec![issue(1, "Still here", &["map"], 0, 60)],
    ));
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);

    assert!(screen.contains("octocat/new-name"), "{screen}");
    assert!(!screen.contains("old-name"), "{screen}");
}

#[test]
fn j_k_arrows_and_g_move_the_selection() {
    let repo = FixtureRepo::with_origin("git@github.com:nyanyaon/github-issue-herdr-plugin.git");
    let stub = StubGithub::serving(issue_list(
        "nyanyaon/github-issue-herdr-plugin",
        three_issues(),
    ));
    let mut app = App::start(&environment(&repo.path, &stub));

    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));

    press(&mut app, KeyCode::Char('j'));
    assert!(selected_line(&screen(&app, 72, 10)).contains("#42"));

    press(&mut app, KeyCode::Char('k'));
    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));

    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert!(selected_line(&screen(&app, 72, 10)).contains("#11"));

    press(&mut app, KeyCode::Up);
    assert!(selected_line(&screen(&app, 72, 10)).contains("#42"));

    press(&mut app, KeyCode::Char('G'));
    assert!(selected_line(&screen(&app, 72, 10)).contains("#11"));

    press(&mut app, KeyCode::Char('g'));
    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));

    // The selection stops at the ends of the list.
    press(&mut app, KeyCode::Up);
    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));
}

#[test]
fn herdr_reserved_keys_are_never_consumed() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list(
        "nyanyaon/github-issue-herdr-plugin",
        three_issues(),
    ));
    let mut app = App::start(&environment(&repo.path, &stub));

    for code in [KeyCode::Char('b'), KeyCode::Char('v'), KeyCode::Char('q')] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
    }
    // Nothing moved, nothing closed: ctrl+b and ctrl+v belong to herdr.
    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));
    assert!(!app.should_exit());

    // Unbound plain keys do nothing either.
    for code in [KeyCode::Char('r'), KeyCode::Char('o'), KeyCode::Tab] {
        press(&mut app, code);
    }
    assert!(selected_line(&screen(&app, 72, 10)).contains("#7"));
    assert!(!app.should_exit());

    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_exit());
}

#[test]
fn a_narrow_pane_clips_the_title_and_keeps_the_row_readable() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list(
        "nyanyaon/github-issue-herdr-plugin",
        three_issues(),
    ));
    let app = App::start(&environment(&repo.path, &stub));

    let wide = screen(&app, 100, 10);
    assert!(
        line_with(&wide, "#42").contains("Walking skeleton: issue list in a pane"),
        "{wide}"
    );

    // The same app after a SIGWINCH to a half-width pane.
    let narrow = screen(&app, 40, 10);
    let row = line_with(&narrow, "#42");
    assert!(row.contains("Walking skeleton"), "{row}");
    assert!(
        row.contains('…'),
        "the title is clipped, not dropped: {row}"
    );
    assert!(row.ends_with("2h"), "{row}");
    assert!(
        narrow.lines().all(|line| line.chars().count() <= 40),
        "nothing overflows the pane:\n{narrow}"
    );
}

#[test]
fn a_repo_with_no_open_issues_says_so() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list("nyanyaon/github-issue-herdr-plugin", vec![]));
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("nyanyaon/github-issue-herdr-plugin · 0 open"),
        "{screen}"
    );
    assert!(screen.contains("no open issues"), "{screen}");
}

#[test]
fn a_workspace_with_no_git_repo_says_so() {
    let workspace = FixtureRepo::not_a_repo();
    let stub = StubGithub::serving(issue_list("nyanyaon/never-asked", vec![]));
    let app = App::start(&environment(&workspace, &stub));

    let screen = screen(&app, 72, 10);
    assert!(screen.contains("no git repo in this workspace"), "{screen}");
    assert!(
        screen.contains(&workspace.display().to_string()),
        "the directory is named: {screen}"
    );
}

#[test]
fn a_remote_on_another_host_is_named_as_unsupported() {
    let repo = FixtureRepo::with_origin("git@gitlab.com:nyanyaon/thing.git");
    let stub = StubGithub::serving(issue_list("nyanyaon/never-asked", vec![]));
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("gitlab.com is not supported — github.com only"),
        "{screen}"
    );
}

#[test]
fn a_rejected_token_says_so_rather_than_that_the_repo_is_missing() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::responding(401, r#"{"message":"Bad credentials"}"#);
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);
    assert!(screen.contains("token rejected"), "{screen}");
}

#[test]
fn a_repo_the_token_cannot_see_is_reported_as_not_found() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/private-thing");
    let stub = StubGithub::serving(
        json!({
            "data": { "repository": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository" }]
        })
        .to_string(),
    );
    let app = App::start(&environment(&repo.path, &stub));

    let screen = screen(&app, 72, 10);
    assert!(
        screen.contains("nyanyaon/private-thing not found — or your token can't see it"),
        "{screen}"
    );
}

#[test]
fn with_no_token_the_pane_says_where_to_get_one() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let stub = StubGithub::serving(issue_list("nyanyaon/never-asked", vec![]));
    let mut environment = environment(&repo.path, &stub);
    environment.token = None;
    let app = App::start(&environment);

    let screen = screen(&app, 72, 10);
    assert!(screen.contains("no GitHub token found"), "{screen}");
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
