//! ADR-0005's discovery order: `GITHUB_TOKEN`, `GH_TOKEN`, `gh auth token`,
//! `token_file`, then the line naming all three ways to supply one.
//!
//! The two legs below `gh` are driven through the app seam against a real
//! `config.toml` and a real token file, and the token that came out is read off
//! the `Authorization` header the stub server received — the only place a
//! resolved token is observable from outside.
//!
//! The three legs above it cannot be driven that way. Setting a process
//! variable is `unsafe` in edition 2024 and races every other test in the
//! binary, and the machine running these tests has a real `gh` on `PATH` whose
//! answer nothing here controls. So the order itself is a function over sources
//! its caller supplies — the one production code calls — and the tests supply
//! them literally. What is not covered is one expression in
//! `Environment::from_process` naming the two variables, and one `gh auth token`
//! spawn.

mod support;

use std::cell::Cell;
use std::fs;
use std::path::Path;

use herdr_issues::environment::{Environment, resolve_token};
use serde_json::json;
use support::{
    ConfigDir, FixtureRepo, StateDir, StubGithub, environment, screen, seconds_ago, start,
};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

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
                        "updatedAt": seconds_ago(2 * 3_600),
                        "comments": { "totalCount": 0 },
                        "author": { "login": "nyanyaon" },
                        "labels": { "nodes": [] },
                    }],
                }
            }
        }
    })
    .to_string()
}

/// A `gh auth token` that would answer, and a flag saying whether it was ever
/// asked — which is how a test sees that a variable stopped the order before
/// the spawn.
fn gh_answering<'a>(
    token: &'static str,
    asked: &'a Cell<bool>,
) -> impl FnOnce() -> Option<String> + 'a {
    move || {
        asked.set(true);
        Some(token.to_string())
    }
}

#[test]
fn github_token_wins_over_everything_below_it() {
    let asked = Cell::new(false);

    let token = resolve_token(
        Some("from-github-token".to_string()),
        Some("from-gh-token".to_string()),
        gh_answering("from-gh-cli", &asked),
    );

    assert_eq!(token.as_deref(), Some("from-github-token"));
    assert!(
        !asked.get(),
        "the environment wins outright, and `gh` is not spawned at all"
    );
}

#[test]
fn gh_token_comes_next_and_still_beats_the_gh_cli() {
    let asked = Cell::new(false);

    let token = resolve_token(
        None,
        Some("from-gh-token".to_string()),
        gh_answering("from-gh-cli", &asked),
    );

    assert_eq!(token.as_deref(), Some("from-gh-token"));
    assert!(!asked.get(), "nor here");
}

#[test]
fn the_gh_cli_answers_when_neither_variable_did() {
    let asked = Cell::new(false);

    let token = resolve_token(None, None, gh_answering("from-gh-cli", &asked));

    assert_eq!(token.as_deref(), Some("from-gh-cli"));
    assert!(asked.get(), "which is the one case that spawns it");
}

#[test]
fn a_blank_variable_is_not_a_token_and_the_order_carries_on_past_it() {
    let asked = Cell::new(false);

    // A container that exports `GITHUB_TOKEN=` unset-but-present, and a
    // `GH_TOKEN` that is nothing but whitespace.
    let token = resolve_token(
        Some(String::new()),
        Some("   \n".to_string()),
        gh_answering("from-gh-cli", &asked),
    );

    assert_eq!(token.as_deref(), Some("from-gh-cli"));
    assert!(asked.get());
}

#[test]
fn a_token_is_trimmed_wherever_it_came_from() {
    assert_eq!(
        resolve_token(Some("  padded  \n".to_string()), None, || None).as_deref(),
        Some("padded")
    );
    // `gh auth token` prints its answer with a trailing newline.
    assert_eq!(
        resolve_token(None, None, || Some("from-gh-cli\n".to_string())).as_deref(),
        Some("from-gh-cli")
    );
}

#[test]
fn nothing_above_the_file_answering_leaves_the_file_to_answer() {
    assert_eq!(resolve_token(None, None, || None), None);
}

/// The fourth leg, through the app seam: a real `config.toml` naming a real
/// file, and the token that reached the wire.
#[test]
fn the_configured_file_supplies_the_token_when_nothing_above_it_did() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::empty();
    let token_file = config.file("gh-issues", "file-token\n");
    config.file(
        "config.toml",
        &format!("token_file = {:?}\n", token_file.display().to_string()),
    );

    let stub = StubGithub::serving(issue_list());
    let mut environment = environment(&repo.path, &stub);
    environment.token = None;
    let app = start(&environment.with_config(config.config()));

    assert_eq!(stub.authorization(0), "Bearer file-token");
    assert!(screen(&app, 72, 10).contains("Pane UI shape"));
}

/// And the file is *last*: a token the three legs above it already resolved is
/// the one that goes on the wire, however good the file looks.
#[test]
fn the_configured_file_does_not_outrank_a_token_already_resolved() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::empty();
    let token_file = config.file("gh-issues", "file-token\n");
    config.file(
        "config.toml",
        &format!("token_file = {:?}\n", token_file.display().to_string()),
    );

    let stub = StubGithub::serving(issue_list());
    // What `$GITHUB_TOKEN`, `$GH_TOKEN` or `gh auth token` left behind.
    let environment = Environment {
        token: Some("from-above-the-file".to_string()),
        ..environment(&repo.path, &stub)
    };
    start(&environment.with_config(config.config()));

    assert_eq!(stub.authorization(0), "Bearer from-above-the-file");
}

/// The token file is read, and only read. A credential the user owns stays a
/// credential the user owns.
#[test]
fn the_viewer_never_writes_a_credential_of_its_own() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::empty();
    let contents = "file-token\nand a note under it\n";
    let token_file = config.file("gh-issues", contents);
    config.file(
        "config.toml",
        &format!("token_file = {:?}\n", token_file.display().to_string()),
    );
    let before = listing(&config.path);
    let state = StateDir::empty();

    let stub = StubGithub::serving(issue_list());
    let mut environment = environment(&repo.path, &stub);
    environment.token = None;
    environment.state_dir = Some(state.path.clone());
    start(&environment.with_config(config.config()));
    assert_eq!(
        stub.authorization(0),
        "Bearer file-token",
        "the token was used"
    );

    assert_eq!(
        fs::read_to_string(&token_file).expect("the token file is still there"),
        contents,
        "the file the user owns comes back byte for byte"
    );
    assert_eq!(
        listing(&config.path),
        before,
        "and the viewer wrote nothing of its own beside it"
    );
    for name in listing(&state.path) {
        let bytes = fs::read(state.path.join(&name)).expect("read a state file");
        assert!(
            !contains(&bytes, b"file-token"),
            "the token is nowhere in {name}: the viewer holds no credential of its own"
        );
    }
}

/// Is `needle` anywhere in `haystack`?
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Every file in a directory, by name, sorted.
fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("read the directory")
        .map(|entry| entry.expect("a directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
