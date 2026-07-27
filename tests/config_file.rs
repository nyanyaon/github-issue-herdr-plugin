//! `config.toml`, driven the way startup meets it: a real file in a real config
//! directory, parsed by the real parser and folded into the environment the app
//! is built from.
//!
//! Most assertions are on what appears on screen; the page sizes are asserted on
//! what went out on the wire, since a page size is a query argument and nothing
//! else. The prune ages are the one exception in this file and say why.

mod support;

use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::environment::Environment;
use serde_json::{Value, json};
use support::{ConfigDir, FixtureRepo, StubGithub, environment, screen, seconds_ago};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";

/// A canned answer to the issue list query, with one issue in it.
fn issue_list() -> String {
    json!({
        "data": {
            "repository": {
                "nameWithOwner": SLUG,
                "issues": {
                    "totalCount": 1,
                    "pageInfo": { "hasNextPage": false, "endCursor": null },
                    "nodes": [row()],
                }
            }
        }
    })
    .to_string()
}

fn row() -> Value {
    json!({
        "number": 7,
        "title": "Pane UI shape",
        "state": "OPEN",
        "updatedAt": seconds_ago(2 * 3_600),
        "comments": { "totalCount": 0 },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
    })
}

/// A canned answer to the detail query for that issue.
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
                    "updatedAt": seconds_ago(2 * 3_600),
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": 0,
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [],
                    }
                }
            }
        }
    })
    .to_string()
}

/// One stub answering both queries, so `enter` reaches a detail request whose
/// page size a test can read.
fn stub() -> StubGithub {
    StubGithub::routing(vec![
        ("\"number\":7".to_string(), issue_detail()),
        ("issues(first:".to_string(), issue_list()),
    ])
}

/// A GitHub that knows no repo, whose not-found line names the slug it was
/// asked for — which is how these tests read a resolved identity.
fn github_that_knows_nothing() -> StubGithub {
    StubGithub::serving(
        json!({
            "data": { "repository": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository" }]
        })
        .to_string(),
    )
}

fn assert_queried(screen: &str, slug: &str) {
    assert!(
        screen.contains(&format!("{slug} not found — or your token can't see it")),
        "expected the viewer to have queried {slug}, got:\n{screen}"
    );
}

/// Wide enough that a status line naming a path is never clipped.
fn pane(environment: &Environment) -> String {
    screen(&App::start(environment), 120, 12)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// Drops whitespace, so an assertion about `"first": 50` does not depend on how
/// the client spaced its JSON.
fn squeeze(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// The repo root as the viewer resolves it, which is the key an override has to
/// carry.
fn repo_root_key(repo_root: &Path) -> String {
    fs::canonicalize(repo_root)
        .expect("the fixture repo root")
        .display()
        .to_string()
}

/// A config file overriding the slug for one repo root.
fn override_config(repo_root: &str, slug: &str) -> ConfigDir {
    ConfigDir::holding(&format!("[repo.{repo_root:?}]\nslug = {slug:?}\n"))
}

/// A config file overriding the slug for this repo root.
fn override_for(repo_root: &Path, slug: &str) -> ConfigDir {
    override_config(&repo_root_key(repo_root), slug)
}

#[test]
fn with_no_config_file_the_documented_defaults_apply_and_nothing_is_reported() {
    let repo = FixtureRepo::with_origin(REMOTE);
    // A fresh install: the directory exists, the file does not.
    let config = ConfigDir::empty();
    let stub = stub();
    let mut app = App::start(&environment(&repo.path, &stub).with_config(config.config()));
    press(&mut app, KeyCode::Enter);

    let list = squeeze(&stub.request(0));
    let detail = squeeze(&stub.request(1));
    assert!(list.contains("\"first\":50"), "{list}");
    assert!(detail.contains("\"first\":100"), "{detail}");

    let screen = screen(&app, 120, 12);
    assert!(
        !screen.contains("config.toml"),
        "needing no configuration is not a problem to report: {screen}"
    );
}

#[test]
fn the_page_sizes_come_from_the_file() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::holding("list_page_size = 5\ndetail_comment_page_size = 7\n");
    let stub = stub();
    let mut app = App::start(&environment(&repo.path, &stub).with_config(config.config()));
    press(&mut app, KeyCode::Enter);

    let list = squeeze(&stub.request(0));
    let detail = squeeze(&stub.request(1));
    assert!(list.contains("\"first\":5"), "{list}");
    assert!(detail.contains("\"first\":7"), "{detail}");
}

#[test]
fn the_prune_ages_are_parsed_and_carried_for_a_prune_that_is_a_later_ticket() {
    // Nothing prunes yet and nothing renders these, so the environment they land
    // in is the only place they are visible. Every other key in this file is
    // asserted through the screen or the wire.
    let config = ConfigDir::holding("prune_details_after_days = 7\nprune_repos_after_days = 14\n");
    let configured = Environment::default().with_config(config.config());
    assert_eq!(configured.prune_details_after_days, 7);
    assert_eq!(configured.prune_repos_after_days, 14);

    let defaults = Environment::default().with_config(ConfigDir::empty().config());
    assert_eq!(defaults.prune_details_after_days, 30);
    assert_eq!(defaults.prune_repos_after_days, 90);
}

#[test]
fn token_file_supplies_the_token_when_nothing_above_it_did() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::empty();
    // The first line, trimmed — a file the user already owns, which the viewer
    // only ever reads.
    let token_file = config.file("gh-issues", "  file-token  \nand a note under it\n");
    config.file(
        "config.toml",
        &format!("token_file = {:?}\n", token_file.display().to_string()),
    );

    let stub = stub();
    let mut environment = environment(&repo.path, &stub);
    // Neither variable is set and `gh` had nothing, which is the only situation
    // in which the file is consulted at all.
    environment.token = None;
    let app = App::start(&environment.with_config(config.config()));

    assert_eq!(stub.authorization(0), "Bearer file-token");
    let screen = screen(&app, 120, 12);
    assert!(screen.contains("Pane UI shape"), "{screen}");
}

#[test]
fn with_no_token_and_no_token_file_the_line_names_all_three_ways() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::empty();
    let stub = stub();
    let mut environment = environment(&repo.path, &stub);
    environment.token = None;

    let screen = pane(&environment.with_config(config.config()));

    assert!(
        screen.contains(
            "no GitHub token found · set GITHUB_TOKEN, run `gh auth login`, or set token_file"
        ),
        "{screen}"
    );
}

#[test]
fn the_override_is_how_a_checkout_with_no_remote_gets_an_identity() {
    let repo = FixtureRepo::empty();
    let config = override_for(&repo.path, "nyanyaon/landcat-extension");
    let stub = github_that_knows_nothing();

    // Without the file there is nothing to go on, and the line says so — and
    // now naming `config.toml` is a remedy that exists.
    let bare = pane(&environment(&repo.path, &stub));
    assert!(
        bare.contains("no git remote · set slug in config.toml"),
        "{bare}"
    );

    let screen = pane(&environment(&repo.path, &stub).with_config(config.config()));
    assert_queried(&screen, "nyanyaon/landcat-extension");
}

#[test]
fn the_override_is_keyed_by_repo_root_exactly_and_beats_the_remote() {
    let repo = FixtureRepo::with_origin("https://github.com/octocat/from-the-remote.git");
    let stub = github_that_knows_nothing();

    let config = override_for(&repo.path, "upstream-org/project");
    assert_queried(
        &pane(&environment(&repo.path, &stub).with_config(config.config())),
        "upstream-org/project",
    );

    // A key that is not this repo root — a subdirectory of it — is not a match,
    // and the remote answers instead.
    let elsewhere = override_config(
        &format!("{}/src", repo_root_key(&repo.path)),
        "upstream-org/project",
    );
    assert_queried(
        &pane(&environment(&repo.path, &stub).with_config(elsewhere.config())),
        "octocat/from-the-remote",
    );
}

#[test]
fn an_override_that_is_not_owner_slash_repo_warns_and_leaves_the_remote_in_charge() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = override_for(&repo.path, "https://github.com/upstream-org/project");
    // A stub that answers, so nothing more urgent than the warning wants the
    // status line; the repo the viewer settled on is read off the query it sent.
    let stub = stub();
    let screen = pane(&environment(&repo.path, &stub).with_config(config.config()));

    let list = squeeze(&stub.request(0));
    assert!(
        list.contains("\"name\":\"github-issue-herdr-plugin\"")
            && list.contains("\"owner\":\"nyanyaon\""),
        "the remote still named the repo: {list}"
    );
    assert!(
        screen.contains(&format!(
            "config.toml · [repo.{:?}] slug is not owner/repo",
            repo_root_key(&repo.path)
        )),
        "the ignored override is named: {screen}"
    );
}

#[test]
fn a_malformed_file_falls_back_to_defaults_with_a_warning_rather_than_refusing_to_start() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::holding("list_page_size = 5\nthis is not toml at all\n");
    let stub = stub();
    let app = App::start(&environment(&repo.path, &stub).with_config(config.config()));

    // The pane opened, the list is on screen, and the file's one good key went
    // with the rest of it: a file that will not parse is not half-applied.
    let screen = screen(&app, 120, 12);
    assert!(screen.contains("Pane UI shape"), "{screen}");
    assert!(
        screen.contains("config.toml · malformed, using defaults"),
        "{screen}"
    );
    let list = squeeze(&stub.request(0));
    assert!(list.contains("\"first\":50"), "{list}");
}

#[test]
fn an_unknown_key_warns_and_is_ignored_while_the_rest_of_the_file_still_applies() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::holding("colour = \"blue\"\nlist_page_size = 3\n");
    let stub = stub();
    let app = App::start(&environment(&repo.path, &stub).with_config(config.config()));

    let list = squeeze(&stub.request(0));
    assert!(list.contains("\"first\":3"), "{list}");

    // The fetch succeeded, so nothing more urgent wants the status line — and
    // the warning is still on it rather than having been overwritten.
    let screen = screen(&app, 120, 12);
    assert!(screen.contains("Pane UI shape"), "{screen}");
    assert!(
        screen.contains("config.toml · ignoring unknown key colour"),
        "{screen}"
    );
}

#[test]
fn a_key_carrying_the_wrong_kind_of_value_costs_only_itself() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let config = ConfigDir::holding("list_page_size = \"fifty\"\ndetail_comment_page_size = 7\n");
    let stub = stub();
    let mut app = App::start(&environment(&repo.path, &stub).with_config(config.config()));
    press(&mut app, KeyCode::Enter);

    let list = squeeze(&stub.request(0));
    let detail = squeeze(&stub.request(1));
    assert!(list.contains("\"first\":50"), "{list}");
    assert!(detail.contains("\"first\":7"), "{detail}");

    let screen = screen(&app, 120, 12);
    assert!(
        screen.contains("config.toml · list_page_size must be a positive whole number"),
        "{screen}"
    );
}
