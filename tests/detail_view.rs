//! The detail view, driven the way a user drives it: `enter` on a row, then
//! `esc`, `n`, `p`, `j` and `k`.
//!
//! Every assertion is on what appears on screen — including every markdown
//! construct, which is asserted as the text it renders to.

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use serde_json::{Value, json};
use support::{FixtureRepo, StubGithub, environment, screen, seconds_ago};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// A canned answer to the issue list query, in GitHub's response shape.
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

fn row(number: u64, title: &str) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": seconds_ago(2 * 3_600),
        "comments": { "totalCount": 0 },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": "grilling", "color": "1d76db" }] },
    })
}

/// A canned answer to the detail query.
fn detail(number: u64, title: &str, body: &str, comments: Vec<Value>) -> String {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "number": number,
                    "title": title,
                    "body": body,
                    "state": "OPEN",
                    "createdAt": seconds_ago(3 * 86_400),
                    "updatedAt": seconds_ago(2 * 3_600),
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "grilling", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": comments.len(),
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": comments,
                    }
                }
            }
        }
    })
    .to_string()
}

fn comment(author: &str, seconds: i64, body: &str) -> Value {
    json!({
        "author": { "login": author },
        "createdAt": seconds_ago(seconds),
        "body": body,
    })
}

/// One stub serving the list query and a detail query per issue number. The
/// detail request carries its number in the variables, which is what routes it.
fn stub(list: String, details: Vec<(u64, String)>) -> StubGithub {
    let mut rules: Vec<(String, String)> = details
        .into_iter()
        .map(|(number, body)| (format!("\"number\":{number}"), body))
        .collect();
    rules.push(("issues(first:".to_string(), list));
    StubGithub::routing(rules)
}

/// An app on a repo with one issue, whose body is the given markdown.
fn app_showing(body: &str) -> (FixtureRepo, StubGithub, App) {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![row(8, "Packaging and distribution")]),
        vec![(8, detail(8, "Packaging and distribution", body, vec![]))],
    );
    let mut app = App::start(&environment(&repo.path, &stub));
    press(&mut app, KeyCode::Enter);
    (repo, stub, app)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// The content region of the detail view: everything between the two rules,
/// so an assertion about the rendered markdown cannot be answered by the
/// header (which carries a `#` before the issue number) or the key hints.
fn body_of(screen: &str) -> String {
    screen
        .lines()
        .skip_while(|line| !line.starts_with('─'))
        .skip(1)
        .take_while(|line| !line.starts_with('─'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{screen}"))
}

#[test]
fn enter_opens_the_detail_view_and_esc_returns_with_the_selection_intact() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![
            row(7, "Pane UI shape"),
            row(42, "Walking skeleton"),
            row(11, "Token discovery"),
        ]),
        vec![(
            42,
            detail(42, "Walking skeleton", "The pane, end to end.", vec![]),
        )],
    );
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);

    let opened = screen(&app, 72, 12);
    assert!(opened.contains("‹ #42 Walking skeleton"), "{opened}");
    assert!(opened.contains("The pane, end to end."), "{opened}");
    assert!(
        opened.contains("esc back   j/k scroll   n/p next issue   q close"),
        "{opened}"
    );
    // The list is gone, not drawn behind the detail.
    assert!(!opened.contains("Token discovery"), "{opened}");

    press(&mut app, KeyCode::Esc);

    let back = screen(&app, 72, 12);
    assert!(back.contains("Token discovery"), "{back}");
    assert!(
        line_with(&back, "▸").contains("#42"),
        "the selection survived the trip: {back}"
    );
}

#[test]
fn n_and_p_walk_the_list_order_without_returning_to_the_list() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![
            row(7, "Pane UI shape"),
            row(42, "Walking skeleton"),
            row(11, "Token discovery"),
        ]),
        vec![
            (7, detail(7, "Pane UI shape", "One column.", vec![])),
            (42, detail(42, "Walking skeleton", "End to end.", vec![])),
            (
                11,
                detail(11, "Token discovery", "GITHUB_TOKEN first.", vec![]),
            ),
        ],
    );
    let mut app = App::start(&environment(&repo.path, &stub));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 72, 12).contains("‹ #7 Pane UI shape"));

    // Forward, in the list's order, without a trip back to the list.
    press(&mut app, KeyCode::Char('n'));
    let second = screen(&app, 72, 12);
    assert!(second.contains("‹ #42 Walking skeleton"), "{second}");
    assert!(second.contains("End to end."), "{second}");
    assert!(
        !second.contains("j/k move"),
        "still in the detail: {second}"
    );

    press(&mut app, KeyCode::Char('n'));
    assert!(screen(&app, 72, 12).contains("‹ #11 Token discovery"));

    // And back the other way.
    press(&mut app, KeyCode::Char('p'));
    assert!(screen(&app, 72, 12).contains("‹ #42 Walking skeleton"));
    press(&mut app, KeyCode::Char('p'));
    assert!(screen(&app, 72, 12).contains("‹ #7 Pane UI shape"));

    // The ends of the list stop the walk rather than wrapping it.
    press(&mut app, KeyCode::Char('p'));
    assert!(screen(&app, 72, 12).contains("‹ #7 Pane UI shape"));

    // esc returns to the issue the walk ended on.
    press(&mut app, KeyCode::Esc);
    assert!(line_with(&screen(&app, 72, 12), "▸").contains("#7"));
}

#[test]
fn the_header_carries_number_title_state_label_author_age_and_comment_count() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![row(8, "Packaging and distribution")]),
        vec![(
            8,
            detail(
                8,
                "Packaging and distribution",
                "Part of #2",
                vec![
                    comment("octocat", 30 * 60, "Prebuilt binaries, then."),
                    comment("nyanyaon", 5 * 60, "Agreed."),
                ],
            ),
        )],
    );
    let mut app = App::start(&environment(&repo.path, &stub));
    press(&mut app, KeyCode::Enter);

    let screen = screen(&app, 72, 14);
    let title = line_with(&screen, "‹ #8");
    assert!(title.contains("Packaging and distribution"), "{title}");

    let meta = line_with(&screen, "open ·");
    assert!(meta.contains("grilling"), "the first label: {meta}");
    assert!(meta.contains("nyanyaon"), "the author: {meta}");
    assert!(meta.contains("2h ago"), "the age: {meta}");
    assert!(meta.contains("2 comments"), "the comment count: {meta}");
    assert!(meta.contains("fetched just now"), "the data age: {meta}");
}

#[test]
fn comments_render_in_order_behind_an_author_and_timestamp_rule() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![row(8, "Packaging and distribution")]),
        vec![(
            8,
            detail(
                8,
                "Packaging and distribution",
                "The body.",
                vec![
                    comment("octocat", 30 * 60, "First, chronologically."),
                    comment("nyanyaon", 5 * 60, "Second, chronologically."),
                ],
            ),
        )],
    );
    let mut app = App::start(&environment(&repo.path, &stub));
    press(&mut app, KeyCode::Enter);

    let screen = screen(&app, 72, 16);
    assert!(
        line_with(&screen, "── octocat").contains("30m ago"),
        "{screen}"
    );
    assert!(
        line_with(&screen, "── nyanyaon").contains("5m ago"),
        "{screen}"
    );
    assert!(
        screen.find("The body.") < screen.find("First, chronologically."),
        "the body comes before the thread:\n{screen}"
    );
    assert!(
        screen.find("First, chronologically.") < screen.find("Second, chronologically."),
        "comments keep their original order:\n{screen}"
    );
}

#[test]
fn headings_and_bold_render_without_their_markers() {
    let (_repo, _stub, app) = app_showing("## Question\n\nWhether to **vendor** SQLite.");

    let screen = screen(&app, 72, 12);
    // The heading is its text, bold — the `##` is chrome the terminal cannot
    // draw, so it is dropped rather than shown.
    assert!(line_with(&screen, "Question") == " Question", "{screen}");
    assert!(
        !body_of(&screen).contains('#'),
        "no heading markers: {screen}"
    );
    assert!(screen.contains("Whether to vendor SQLite."), "{screen}");
    assert!(!screen.contains("**"), "no emphasis markers: {screen}");
}

#[test]
fn lists_are_bulleted_with_a_hang_indent() {
    let (_repo, _stub, app) = app_showing(
        "- a bullet long enough that it has to wrap onto a second line of the pane\n- another\n\n1. first\n2. second",
    );

    let screen = screen(&app, 40, 14);
    let bullet = line_with(&screen, "a bullet");
    assert!(bullet.starts_with(" • a bullet"), "{bullet:?}");
    // The wrapped remainder hangs under the text, not under the bullet.
    let continuation = line_with(&screen, "pane");
    assert!(
        continuation.starts_with("   ") && !continuation.contains('•'),
        "hang indent: {continuation:?}"
    );
    assert!(
        line_with(&screen, "another").starts_with(" • another"),
        "{screen}"
    );
    // An ordered list keeps its numbers, because the numbers are the content.
    assert!(
        line_with(&screen, "1. first").starts_with(" 1. first"),
        "{screen}"
    );
    assert!(
        line_with(&screen, "2. second").starts_with(" 2. second"),
        "{screen}"
    );
}

#[test]
fn fenced_code_sits_behind_a_gutter_and_inline_code_loses_its_backticks() {
    let (_repo, _stub, app) = app_showing(
        "Run `cargo build --release` first.\n\n```sh\nherdr plugin link .\n```\n\nThen open the pane.",
    );

    let screen = screen(&app, 72, 14);
    assert!(
        screen.contains("Run cargo build --release first."),
        "{screen}"
    );
    assert!(!screen.contains('`'), "no backticks survive: {screen}");
    let code = line_with(&screen, "herdr plugin link .");
    assert!(code.starts_with(" │ herdr plugin link ."), "{code:?}");
    // The fence markers themselves are chrome, not content.
    assert!(!screen.contains("sh\n"), "{screen}");
    assert!(screen.contains("Then open the pane."), "{screen}");
}

#[test]
fn links_render_as_text_with_their_target_after_it() {
    let (_repo, _stub, app) = app_showing("See [the ADR](https://example.com/adr) for why.");

    let screen = screen(&app, 72, 12);
    assert!(
        screen.contains("See the ADR (https://example.com/adr) for why."),
        "{screen}"
    );
    assert!(!screen.contains("]("), "no link syntax survives: {screen}");
}

#[test]
fn tables_and_images_degrade_to_their_raw_source() {
    let (_repo, _stub, app) = app_showing(
        "| crate | why |\n|---|---|\n| ureq | blocking HTTP |\n\n![the pane](https://example.com/pane.png)",
    );

    let screen = screen(&app, 72, 14);
    // A table is its own source: no alignment engine, no bad rendering.
    assert!(screen.contains("| crate | why |"), "{screen}");
    assert!(screen.contains("| ureq | blocking HTTP |"), "{screen}");
    // An image is its markdown, which at least says what is missing.
    assert!(
        screen.contains("![the pane](https://example.com/pane.png)"),
        "{screen}"
    );
}

#[test]
fn the_body_wraps_to_the_pane_and_rewraps_on_resize() {
    let sentence = "The pane must be cheap enough that several are open at once on a loaded laptop, which is the whole point of not opening a browser tab.";
    let (_repo, _stub, app) = app_showing(&format!(
        "{sentence}\n\n```\nan-unbroken-line-of-code-far-wider-than-any-narrow-pane-would-ever-be\n```"
    ));

    for width in [100u16, 72, 40, 24] {
        let screen = screen(&app, width, 30);
        assert!(
            screen
                .lines()
                .all(|line| line.chars().count() <= width as usize),
            "nothing overflows a {width}-column pane:\n{screen}"
        );
        // Every word survives the wrap, at every width.
        let flattened = screen.replace('\n', " ");
        for word in sentence.split_whitespace() {
            assert!(
                flattened.contains(word),
                "{word:?} lost at width {width}:\n{screen}"
            );
        }
    }
}

#[test]
fn j_and_k_scroll_the_detail_view_and_stop_at_its_end() {
    let body: String = (1..=40)
        .map(|n| format!("line-{n}\n\n"))
        .collect::<Vec<_>>()
        .join("");
    let (_repo, _stub, mut app) = app_showing(&body);

    assert!(screen(&app, 72, 10).contains("line-1"), "the top first");

    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    let scrolled = screen(&app, 72, 10);
    assert!(!scrolled.contains("line-1 "), "{scrolled}");
    assert!(scrolled.contains("line-2"), "{scrolled}");

    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Char('k'));
    assert!(screen(&app, 72, 10).contains("line-1"), "back at the top");

    // Scrolling past the end lands on the end, and stays there.
    for _ in 0..200 {
        press(&mut app, KeyCode::Char('j'));
        screen(&app, 72, 10);
    }
    let bottom = screen(&app, 72, 10);
    assert!(
        bottom.contains("line-40"),
        "the last line is on screen:\n{bottom}"
    );

    press(&mut app, KeyCode::Char('k'));
    assert!(screen(&app, 72, 10).contains("line-39"), "one line back up");
}

#[test]
fn a_failed_detail_fetch_leaves_the_list_on_screen() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![row(8, "Packaging and distribution")]),
        vec![(
            8,
            json!({
                "data": { "repository": { "issue": null } },
                "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to an Issue" }]
            })
            .to_string(),
        )],
    );
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Enter);

    let screen = screen(&app, 72, 12);
    assert!(screen.contains("Packaging and distribution"), "{screen}");
    assert!(
        screen.contains("nyanyaon/github-issue-herdr-plugin#8 not found"),
        "{screen}"
    );
    assert!(
        line_with(&screen, "▸").contains("#8"),
        "still the list: {screen}"
    );
}

#[test]
fn herdr_reserved_keys_are_never_consumed_in_the_detail_view() {
    let (_repo, _stub, mut app) = app_showing("The body.");

    for code in [KeyCode::Char('b'), KeyCode::Char('v'), KeyCode::Char('q')] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL));
    }
    assert!(!app.should_exit());
    assert!(
        screen(&app, 72, 12).contains("‹ #8"),
        "still the detail view"
    );

    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_exit());
}

/// The filter and the detail view were built on separate branches, and the
/// merge had one hazard: the selection counts *visible* rows, so `enter` under
/// a filter must open the row under the cursor rather than the nth fetched one.
///
/// Here `#42` is the second row fetched but the first — and only — row the
/// filter leaves, so an index into the wrong list opens `#7` or nothing at all.
#[test]
fn enter_under_a_filter_opens_the_row_the_cursor_is_on() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![
            row(7, "Pane UI shape"),
            row(42, "Walking skeleton"),
            row(11, "Token discovery"),
        ]),
        vec![
            (7, detail(7, "Pane UI shape", "The wrong issue.", vec![])),
            (
                42,
                detail(42, "Walking skeleton", "The right issue.", vec![]),
            ),
        ],
    );
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Char('/'));
    for character in "wskel".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    // The filter left one row, and the selection sits on it at index 0.
    let filtered = screen(&app, 72, 12);
    assert!(filtered.contains("1 of 3 shown"), "{filtered}");

    press(&mut app, KeyCode::Enter); // leaves typing mode, keeps the filter
    press(&mut app, KeyCode::Enter); // opens the row under the cursor

    let detail_screen = screen(&app, 72, 12);
    assert!(detail_screen.contains("‹ #42"), "{detail_screen}");
    assert!(
        detail_screen.contains("The right issue."),
        "{detail_screen}"
    );
    assert!(
        !detail_screen.contains("The wrong issue."),
        "{detail_screen}"
    );
}

/// `n` from inside the detail view walks the *filtered* order too, so it never
/// steps onto a row the filter is hiding.
#[test]
fn n_walks_the_filtered_order_rather_than_the_fetched_one() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub(
        issue_list(vec![
            row(7, "Skeleton crew"),
            row(42, "Pane UI shape"),
            row(11, "Skeleton key"),
        ]),
        vec![
            (7, detail(7, "Skeleton crew", "First.", vec![])),
            (
                42,
                detail(42, "Pane UI shape", "Hidden by the filter.", vec![]),
            ),
            (11, detail(11, "Skeleton key", "Second.", vec![])),
        ],
    );
    let mut app = App::start(&environment(&repo.path, &stub));

    press(&mut app, KeyCode::Char('/'));
    for character in "skel".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    assert!(screen(&app, 72, 12).contains("2 of 3 shown"));

    press(&mut app, KeyCode::Enter); // leave typing mode
    press(&mut app, KeyCode::Enter); // open #7
    assert!(screen(&app, 72, 12).contains("‹ #7"));

    // #42 sits between them in the fetched list, and the filter hides it.
    press(&mut app, KeyCode::Char('n'));
    let stepped = screen(&app, 72, 12);
    assert!(stepped.contains("‹ #11"), "{stepped}");
    assert!(!stepped.contains("Hidden by the filter."), "{stepped}");

    // And #11 is the last visible row, so `n` stops rather than wrapping.
    press(&mut app, KeyCode::Char('n'));
    assert!(screen(&app, 72, 12).contains("‹ #11"));
}
