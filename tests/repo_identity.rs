//! Repo identity, driven the way a pane meets it: an environment description in,
//! rendered text out.
//!
//! Every assertion here is on what appears on screen. Two of those screens carry
//! the answer indirectly and deliberately: the not-found status line names the
//! slug that was **queried**, so a test can see which repo the viewer decided it
//! was looking at without reaching under the seam.

mod support;

use std::fs;
use std::path::Path;

use herdr_issues::app::App;
use herdr_issues::environment::Environment;
use herdr_issues::identity::{Slug, SlugOverrides};
use serde_json::json;
use support::{FixtureRepo, StubGithub, StubHerdr, environment, screen};

/// A GitHub that knows no repo at all. Its status line names whatever slug it
/// was asked for, which is how these tests read the resolution result.
fn github_that_knows_nothing() -> StubGithub {
    StubGithub::serving(
        json!({
            "data": { "repository": null },
            "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to a Repository" }]
        })
        .to_string(),
    )
}

/// A GitHub with one issue in whatever repo it is asked for.
fn github_with_one_issue() -> StubGithub {
    StubGithub::serving(
        json!({
            "data": {
                "repository": {
                    "nameWithOwner": "nyanyaon/github-issue-herdr-plugin",
                    "issues": {
                        "totalCount": 1,
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [{
                            "number": 19,
                            "title": "Repo identity: socket and full resolution",
                            "state": "OPEN",
                            "updatedAt": "2026-07-27T00:00:00Z",
                            "comments": { "totalCount": 0 },
                            "author": { "login": "nyanyaon" },
                            "labels": { "nodes": [{ "name": "ready-for-agent", "color": "1d76db" }] },
                        }],
                    }
                }
            }
        })
        .to_string(),
    )
}

/// The slug the viewer resolved to and queried, read off the not-found line.
fn assert_queried(screen: &str, slug: &str) {
    assert!(
        screen.contains(&format!("{slug} not found — or your token can't see it")),
        "expected the viewer to have queried {slug}, got:\n{screen}"
    );
}

/// Starts a pane on this environment and returns what it drew. Wide enough that
/// a status line naming a slug or a path is never clipped.
fn pane(environment: &Environment) -> String {
    screen(&App::start(environment), 120, 12)
}

#[test]
fn the_repo_root_comes_from_the_herdr_socket_by_workspace_id() {
    // The workspace directory is a different repo from the one herdr reports —
    // so only an answer from the socket can produce `octocat/from-the-socket`.
    let workspace = FixtureRepo::with_origin("https://github.com/octocat/from-the-directory.git");
    let source = FixtureRepo::with_origin("https://github.com/octocat/from-the-socket.git");
    let herdr = StubHerdr::serving("w3", &source.path);

    let stub = github_that_knows_nothing();
    let mut environment = environment(&workspace.path, &stub);
    environment.herdr_socket = Some(herdr.socket_path.clone());
    environment.workspace_id = Some("w3".to_string());

    let screen = pane(&environment);

    assert_queried(&screen, "octocat/from-the-socket");
    assert!(!screen.contains("from-the-directory"), "{screen}");
}

#[test]
fn a_socket_that_cannot_be_reached_falls_back_to_git() {
    let repo = FixtureRepo::with_origin("https://github.com/octocat/from-git.git");
    let stub = github_that_knows_nothing();
    let mut environment = environment(&repo.path, &stub);
    // Herdr is gone, or the viewer was launched outside it with a stale path.
    environment.herdr_socket = Some(StubHerdr::absent());
    environment.workspace_id = Some("w3".to_string());

    assert_queried(&pane(&environment), "octocat/from-git");
}

#[test]
fn every_worktree_of_a_repo_shows_one_issue_list_over_the_socket() {
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/github-issue-herdr-plugin");
    let worktree = repo.linked_worktree("19-repo-identity-socket");
    // Herdr collapses a linked worktree onto its source repo, so a pane opened
    // in either workspace is told the same repo root.
    let herdr = StubHerdr::serving("w7", &repo.path);

    let stub = github_with_one_issue();
    let mut in_the_worktree = environment(&worktree, &stub);
    in_the_worktree.herdr_socket = Some(herdr.socket_path.clone());
    in_the_worktree.workspace_id = Some("w7".to_string());

    let mut in_the_source = environment(&repo.path, &stub);
    in_the_source.herdr_socket = Some(herdr.socket_path.clone());
    in_the_source.workspace_id = Some("w7".to_string());

    let from_the_worktree = pane(&in_the_worktree);
    assert!(from_the_worktree.contains("#19"), "{from_the_worktree}");
    assert_eq!(
        from_the_worktree,
        pane(&in_the_source),
        "a worktree and its source repo show the same issue list"
    );
}

#[test]
fn a_worktree_resolves_to_its_source_repo_without_a_socket_too() {
    // `--show-toplevel` stops at the worktree; the common dir is what reaches
    // the repo whose remotes name the issues.
    let repo = FixtureRepo::with_origin("https://github.com/nyanyaon/from-the-source-repo");
    let worktree = repo.linked_worktree("19-repo-identity-socket");

    let stub = github_that_knows_nothing();
    let screen = pane(&environment(&worktree, &stub));

    assert_queried(&screen, "nyanyaon/from-the-source-repo");
}

#[test]
fn a_workspace_herdr_has_no_repo_for_names_the_directory() {
    let workspace = FixtureRepo::not_a_repo();
    let herdr = StubHerdr::without_a_repo();

    let stub = github_that_knows_nothing();
    let mut environment = environment(&workspace, &stub);
    environment.herdr_socket = Some(herdr.socket_path.clone());
    environment.workspace_id = Some("w3".to_string());

    let screen = pane(&environment);

    assert!(screen.contains("no git repo in this workspace"), "{screen}");
    assert!(
        screen.contains(&workspace.display().to_string()),
        "the directory is named: {screen}"
    );
}

#[test]
fn the_config_override_wins_over_origin() {
    let repo = FixtureRepo::with_origin("https://github.com/octocat/from-the-remote.git");
    let stub = github_that_knows_nothing();
    let mut environment = environment(&repo.path, &stub);
    environment.slug_overrides = overrides_for(&repo.path, "upstream-org/project");

    assert_queried(&pane(&environment), "upstream-org/project");
}

#[test]
fn the_override_is_how_a_checkout_with_no_remote_gets_an_identity() {
    let repo = FixtureRepo::empty();
    let stub = github_that_knows_nothing();

    // Without one, there is nothing to go on and the line says so.
    let bare = pane(&environment(&repo.path, &stub));
    assert!(
        bare.contains("no git remote · set slug in config.toml"),
        "{bare}"
    );

    let mut environment = environment(&repo.path, &stub);
    environment.slug_overrides = overrides_for(&repo.path, "nyanyaon/landcat-extension");

    assert_queried(&pane(&environment), "nyanyaon/landcat-extension");
}

#[test]
fn origin_names_the_repo_even_when_another_remote_is_upstream() {
    // The `gdal3.js` shape: `origin` is the upstream, and a second remote is the
    // personal fork. `origin` wins, as it is what the pane should read.
    let repo = FixtureRepo::with_remotes(&[
        ("origin", "https://github.com/bugra9/gdal3.js.git"),
        ("nyaon", "git@github.com:nyanyaon/gdal3.js.git"),
    ]);
    let stub = github_that_knows_nothing();

    assert_queried(&pane(&environment(&repo.path, &stub)), "bugra9/gdal3.js");
}

#[test]
fn the_sole_remote_names_the_repo_when_it_is_not_called_origin() {
    let repo = FixtureRepo::with_remotes(&[("nyaon", "git@github.com:nyanyaon/landcat.git")]);
    let stub = github_that_knows_nothing();

    assert_queried(&pane(&environment(&repo.path, &stub)), "nyanyaon/landcat");
}

#[test]
fn several_remotes_and_no_origin_lists_the_candidates_and_names_the_config_key() {
    let repo = FixtureRepo::with_remotes(&[
        ("fork", "https://github.com/nyanyaon/gdal3.js.git"),
        ("upstream", "https://github.com/bugra9/gdal3.js.git"),
    ]);
    let stub = github_that_knows_nothing();

    let screen = pane(&environment(&repo.path, &stub));

    assert!(
        screen.contains("several remotes and no origin: fork, upstream · set slug in config.toml"),
        "{screen}"
    );
}

#[test]
fn every_remote_url_form_names_the_same_repo() {
    for url in [
        "https://github.com/nyanyaon/github-issue-herdr-plugin.git",
        "git@github.com:nyanyaon/github-issue-herdr-plugin.git",
        "ssh://git@github.com/nyanyaon/github-issue-herdr-plugin.git",
    ] {
        let repo = FixtureRepo::with_origin(url);
        let stub = github_that_knows_nothing();
        let screen = pane(&environment(&repo.path, &stub));

        assert_queried(&screen, "nyanyaon/github-issue-herdr-plugin");
    }
}

#[test]
fn a_host_that_is_not_github_is_named_in_every_url_form() {
    for (url, host) in [
        ("https://gitlab.com/nyanyaon/thing.git", "gitlab.com"),
        ("git@codeberg.org:nyanyaon/thing.git", "codeberg.org"),
        ("ssh://git@bitbucket.org/nyanyaon/thing", "bitbucket.org"),
    ] {
        let repo = FixtureRepo::with_origin(url);
        let stub = github_that_knows_nothing();
        let screen = pane(&environment(&repo.path, &stub));

        assert!(
            screen.contains(&format!("{host} is not supported — github.com only")),
            "{screen}"
        );
    }
}

/// The override table as the config file will hand it over: a repo root, exactly
/// as resolved, and a parsed `owner/repo`.
fn overrides_for(repo_root: &Path, slug: &str) -> SlugOverrides {
    let repo_root = fs::canonicalize(repo_root).expect("the fixture repo root");
    SlugOverrides::from_entries([(
        repo_root,
        Slug::parse(slug).expect("a slug this test wrote"),
    )])
}
