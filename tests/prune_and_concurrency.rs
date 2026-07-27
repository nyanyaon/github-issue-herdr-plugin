//! The prune and the shared database, driven the way panes meet them: real
//! SQLite files in real temp directories, real panes writing them, and real
//! threads racing on them.
//!
//! Two things are asserted here that nothing else can assert. The first is
//! *when* the prune runs — SPEC §5 puts the cached frame before anything the
//! pane does for itself, so these tests drive [`App::start`] and
//! [`App::run_pending_query`] separately and look at the file in between. The
//! second is that several panes on one database do not tread on each other,
//! which needs more than one thread to mean anything.
//!
//! The database is opened with SQL where the assertion is about a column rather
//! than about the screen: the access bookkeeping the prune ages rows by is not
//! rendered anywhere, and a test that could only see the screen could not see
//! it at all.

mod support;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use herdr_issues::app::App;
use herdr_issues::cache::{Cache, PrunePolicy};
use herdr_issues::environment::Environment;
use herdr_issues::github::IssueStates;
use herdr_issues::identity::Slug;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use support::{
    ConfigDir, FixtureRepo, StateDir, StubGithub, environment, screen, seconds_ago, settle, start,
};

const REMOTE: &str = "https://github.com/nyanyaon/github-issue-herdr-plugin";
const SLUG: &str = "nyanyaon/github-issue-herdr-plugin";
const OTHER_REMOTE: &str = "https://github.com/octocat/other-thing";
const OTHER_SLUG: &str = "octocat/other-thing";

/// An endpoint with nothing listening on it, so a pane runs off the cache
/// alone — which is what makes surviving rows evidence of a prune's restraint
/// rather than of a re-fetch.
const UNREACHABLE: &str = "http://127.0.0.1:9/graphql";

const DAY: i64 = 86_400;

fn slug(text: &str) -> Slug {
    Slug::parse(text).expect("a slug this test wrote")
}

fn issue(number: u64, title: &str) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": "OPEN",
        "updatedAt": seconds_ago(18 * 60),
        "comments": { "totalCount": 1 },
        "author": { "login": "nyanyaon" },
        "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
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

fn issue_detail_body(number: u64, title: &str) -> String {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "number": number,
                    "title": title,
                    "body": "One column, drill-in.",
                    "state": "OPEN",
                    "createdAt": seconds_ago(3 * DAY),
                    "updatedAt": seconds_ago(18 * 60),
                    "author": { "login": "nyanyaon" },
                    "labels": { "nodes": [{ "name": "prototype", "color": "1d76db" }] },
                    "comments": {
                        "totalCount": 1,
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
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

/// One stub answering both queries for one repo, the way a pane meets GitHub.
fn stub_for(name_with_owner: &str, issues: Vec<(u64, &str)>) -> StubGithub {
    let mut rules: Vec<(String, String)> = issues
        .iter()
        .map(|(number, title)| {
            (
                format!("\"number\":{number}"),
                issue_detail_body(*number, title),
            )
        })
        .collect();
    rules.push((
        "issues(first:".to_string(),
        issue_list_for(
            name_with_owner,
            issues
                .iter()
                .map(|(number, title)| issue(*number, title))
                .collect(),
        ),
    ));
    StubGithub::routing(rules)
}

fn stub() -> StubGithub {
    stub_for(SLUG, vec![(7, "Pane UI shape"), (8, "Packaging")])
}

fn pane(workspace: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        state_dir: Some(state.path.clone()),
        ..environment(workspace, stub)
    }
}

/// The same pane with the network gone.
fn offline_pane(workspace: &Path, stub: &StubGithub, state: &StateDir) -> Environment {
    Environment {
        graphql_url: UNREACHABLE.to_string(),
        ..pane(workspace, stub, state)
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// The database the panes wrote, opened with SQL because the columns the prune
/// reads are not on any screen.
fn database(state: &StateDir) -> Connection {
    Connection::open(state.database()).expect("open the database a pane wrote")
}

fn count(connection: &Connection, table: &str, slug: &str) -> i64 {
    connection
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE slug = ?1"),
            params![slug],
            |row| row.get(0),
        )
        .expect("count the rows")
}

/// How many rows a repo has left, across every table it owns.
fn rows_of(connection: &Connection, slug: &str) -> Vec<(&'static str, i64)> {
    ["repo", "issue_list", "issue_detail", "issue_comments"]
        .into_iter()
        .map(|table| (table, count(connection, table, slug)))
        .collect()
}

fn scalar(connection: &Connection, sql: &str, slug: &str) -> i64 {
    connection
        .query_row(sql, params![slug], |row| row.get(0))
        .expect("read the column")
}

/// Dates the access bookkeeping back, the way months of not opening a pane
/// would. Nothing else can age a row: both columns are written by the viewer.
fn age_back(connection: &Connection, slug: &str, repo_days: i64, detail_days: i64) {
    let now = herdr_issues::age::now();
    connection
        .execute(
            "UPDATE repo SET opened_at = ?2 WHERE slug = ?1",
            params![slug, now - repo_days * DAY],
        )
        .expect("date the repo back");
    connection
        .execute(
            "UPDATE issue_detail SET touched_at = ?2 WHERE slug = ?1",
            params![slug, now - detail_days * DAY],
        )
        .expect("date the detail back");
}

/// A pane that read this repo and opened its first issue, then closed. The
/// state every later pane in this file starts from.
fn seed(workspace: &Path, stub: &StubGithub, state: &StateDir) {
    let mut app = start(&pane(workspace, stub, state));
    press(&mut app, KeyCode::Enter);
    assert!(
        screen(&app, 72, 16).contains("One column, drill-in."),
        "the seeding pane read an issue"
    );
}

/// The first acceptance criterion, and the one that was already half built:
/// `repo.opened_at`, `repo.open_count` and `issue_detail.touched_at` are the
/// three columns the prune ages rows by, and each is written at the moment its
/// name claims.
///
/// The point of the test is the *moments*, not the columns: a pane that dated a
/// repo only when it fetched would let a repo read offline every day for a year
/// be pruned out from under its reader.
#[test]
fn a_pane_dates_the_repo_it_opens_and_the_detail_it_displays() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);

    let connection = database(&state);
    let opened_at = |connection: &Connection| {
        scalar(
            connection,
            "SELECT opened_at FROM repo WHERE slug = ?1",
            SLUG,
        )
    };
    let open_count = |connection: &Connection| {
        scalar(
            connection,
            "SELECT open_count FROM repo WHERE slug = ?1",
            SLUG,
        )
    };
    let touched_at = |connection: &Connection| {
        scalar(
            connection,
            "SELECT touched_at FROM issue_detail WHERE slug = ?1 AND number = 7",
            SLUG,
        )
    };
    assert_eq!(open_count(&connection), 1, "one pane has opened this repo");
    assert!(herdr_issues::age::now() - opened_at(&connection) < 60);

    // Months pass with the pane closed.
    age_back(&connection, SLUG, 40, 40);
    let stale_touch = touched_at(&connection);
    drop(connection);

    // A pane opens on it again, with the network gone — so nothing but the act
    // of opening can be what dates it.
    let mut app = App::start(&offline_pane(&repo.path, &stub, &state));
    let connection = database(&state);
    assert_eq!(open_count(&connection), 2, "opening the pane counted");
    assert!(
        herdr_issues::age::now() - opened_at(&connection) < 60,
        "and dated the repo to now, without a fetch"
    );
    assert_eq!(
        touched_at(&connection),
        stale_touch,
        "but did not date a detail nobody has looked at"
    );

    // Displaying the issue is what dates the detail.
    press(&mut app, KeyCode::Enter);
    assert!(screen(&app, 72, 16).contains("One column, drill-in."));
    assert!(
        herdr_issues::age::now() - touched_at(&database(&state)) < 60,
        "displaying a detail dates it"
    );
}

/// Where the prune sits relative to first paint, asserted rather than claimed.
///
/// SPEC §5 lists the prune at step 4 and the cached frame at step 6, but §12
/// makes first paint the constraint, and the prune is a write — on a shared
/// database it can sit behind another pane's writer for as long as the busy
/// timeout allows. So the frame goes out first and the prune follows it, still
/// on the startup path and still over before the pane blocks on input.
#[test]
fn the_startup_prune_runs_behind_the_first_frame_rather_than_ahead_of_it() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);
    age_back(&database(&state), SLUG, 0, 31);

    let mut app = App::start(&offline_pane(&repo.path, &stub, &state));

    // The frame a user sees first — drawn from rows the prune has not looked at
    // yet.
    let first = screen(&app, 72, 10);
    assert!(first.contains("Pane UI shape"), "{first}");
    assert_eq!(
        count(&database(&state), "issue_detail", SLUG),
        1,
        "the aged detail is still there at the first frame: nothing was deleted ahead of it"
    );
    assert!(app.has_pending_query(), "the prune is queued behind it");

    settle(&mut app);

    let connection = database(&state);
    assert_eq!(
        count(&connection, "issue_detail", SLUG),
        0,
        "and by the time the pane blocks on input it has run"
    );
    assert_eq!(
        count(&connection, "issue_comments", SLUG),
        0,
        "the comment pages went with the body"
    );
    assert_eq!(
        count(&connection, "issue_list", SLUG),
        2,
        "and nothing else did: the list is what the next first frame is drawn from"
    );
    assert!(
        screen(&app, 72, 10).contains("Pane UI shape"),
        "including the frame still on screen"
    );
}

/// A detail read a fortnight ago is not old. The prune deleting it would be the
/// prune deleting the thing the cache is for.
#[test]
fn a_detail_inside_its_age_survives_a_launch() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);
    age_back(&database(&state), SLUG, 14, 14);

    let mut app = start(&offline_pane(&repo.path, &stub, &state));

    let connection = database(&state);
    assert_eq!(
        rows_of(&connection, SLUG),
        vec![
            ("repo", 1),
            ("issue_list", 2),
            ("issue_detail", 1),
            ("issue_comments", 1)
        ]
    );
    press(&mut app, KeyCode::Enter);
    let detail = screen(&app, 72, 16);
    assert!(
        detail.contains("One column, drill-in."),
        "and it opens from the cache with the network gone:\n{detail}"
    );
    assert!(detail.contains("The first of many."), "{detail}");
}

/// The second rule, and the one that spans four tables: a repo nothing has
/// opened in long enough leaves nothing behind — and the repo in the next pane
/// along keeps everything.
#[test]
fn an_unopened_repo_is_dropped_whole_and_the_repo_being_read_is_untouched() {
    let mine = FixtureRepo::with_origin(REMOTE);
    let theirs = FixtureRepo::with_origin(OTHER_REMOTE);
    let state = StateDir::empty();
    let my_stub = stub();
    let their_stub = stub_for(OTHER_SLUG, vec![(3, "Something else entirely")]);
    seed(&mine.path, &my_stub, &state);
    seed(&theirs.path, &their_stub, &state);
    // The other repo has not been opened since the spring; mine, yesterday.
    age_back(&database(&state), OTHER_SLUG, 100, 100);
    age_back(&database(&state), SLUG, 1, 1);

    let app = start(&offline_pane(&mine.path, &my_stub, &state));

    let connection = database(&state);
    assert_eq!(
        rows_of(&connection, OTHER_SLUG),
        vec![
            ("repo", 0),
            ("issue_list", 0),
            ("issue_detail", 0),
            ("issue_comments", 0)
        ],
        "every row of the abandoned repo went"
    );
    assert_eq!(
        rows_of(&connection, SLUG),
        vec![
            ("repo", 1),
            ("issue_list", 2),
            ("issue_detail", 1),
            ("issue_comments", 1)
        ],
        "and nothing of the one being read"
    );
    assert!(screen(&app, 72, 10).contains("Pane UI shape"));

    // The abandoned repo is simply a cold start again, not a corrupt one.
    let cache = Cache::open(Some(&state.path)).expect("the database is still a database");
    assert!(
        cache
            .issue_list(&slug(OTHER_SLUG), IssueStates::All)
            .is_none()
    );
    assert_eq!(
        cache
            .issue_list(&slug(SLUG), IssueStates::Open)
            .expect("mine is still there")
            .rows
            .len(),
        2
    );
}

/// The trap in ageing repos by when they were last opened: the pane doing the
/// pruning is itself an opening. A repo untouched for two hundred days that you
/// open right now is saved by the act of opening it, because `mark_opened` runs
/// with the cached rows and the prune runs after them.
#[test]
fn a_pane_never_prunes_the_repo_it_is_opening() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);
    age_back(&database(&state), SLUG, 200, 1);

    let app = start(&offline_pane(&repo.path, &stub, &state));

    let connection = database(&state);
    assert_eq!(
        rows_of(&connection, SLUG),
        vec![
            ("repo", 1),
            ("issue_list", 2),
            ("issue_detail", 1),
            ("issue_comments", 1)
        ],
        "opening the repo is what saved it"
    );
    assert!(
        screen(&app, 72, 10).contains("Pane UI shape"),
        "and the pane it saved them for still has them on screen"
    );
}

/// The ages are the config file's, not the code's: a detail nine days unread
/// survives the default thirty and goes under a configured seven.
///
/// The same database, the same rows, the same pane — only `config.toml`
/// differs, which is what makes this the key reaching the deletion rather than
/// the deletion happening to work.
#[test]
fn a_configured_age_is_the_one_the_prune_uses() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let stub = stub();
    let aged_nine_days = |ages: &str| {
        let state = StateDir::empty();
        seed(&repo.path, &stub, &state);
        age_back(&database(&state), SLUG, 1, 9);
        let config = ConfigDir::holding(ages);
        let environment = offline_pane(&repo.path, &stub, &state).with_config(config.config());
        drop(start(&environment));
        count(&database(&state), "issue_detail", SLUG)
    };

    assert_eq!(
        aged_nine_days(""),
        1,
        "nine days is well inside the default thirty"
    );
    assert_eq!(
        aged_nine_days("prune_details_after_days = 7\n"),
        0,
        "and outside a configured seven"
    );
}

/// Compaction is the file size's call, not the launch's (SPEC §9).
///
/// A pane's database is kilobytes, so the standard threshold is the case that
/// happens every launch for years: prune, and leave the file alone.
#[test]
fn a_launch_prunes_without_compacting_a_small_file() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);
    age_back(&database(&state), SLUG, 0, 31);

    let cache = Cache::open(Some(&state.path)).expect("open the cache");
    let pruned = cache.prune(PrunePolicy::after_days(30, 90));

    assert_eq!(pruned.details, 1);
    assert!(
        !pruned.compacted,
        "a few kilobytes is not worth rewriting the file for"
    );
    assert!(
        PrunePolicy::after_days(30, 90).compact_above_bytes == 64 * 1024 * 1024,
        "SPEC §9's threshold"
    );
}

/// The claim WAL is chosen for, measured rather than asserted: a reader is not
/// blocked by a writer.
///
/// The writer is a raw connection holding a real write transaction open, which
/// is the only way to stage the moment — a pane's own writes are single
/// transactions that are over too quickly to read across. Both things the
/// reading pane does at startup are timed: *opening* the cache, which takes no
/// write lock once the file is migrated, and reading the list, which takes
/// none ever.
#[test]
fn a_reader_is_not_blocked_while_another_pane_holds_the_write_lock() {
    let repo = FixtureRepo::with_origin(REMOTE);
    let state = StateDir::empty();
    let stub = stub();
    seed(&repo.path, &stub, &state);

    let held = Duration::from_millis(600);
    let path = state.database();
    let (holding, wait) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let writer = thread::spawn(move || {
        let mut connection = Connection::open(&path).expect("the writing pane's connection");
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=3000;")
            .expect("the pragmas a pane opens with");
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("take the write lock");
        transaction
            .execute(
                "INSERT INTO issue_list (slug, number, title, state, updated_at, comment_count,
                                         author, labels_json)
                 VALUES (?1, 99, 'Written while someone was reading', 'OPEN',
                         '2026-07-27T09:14:03Z', 0, 'nyanyaon', '[]')",
                params![SLUG],
            )
            .expect("write while the lock is held");
        holding.send(()).expect("announce the held lock");
        thread::sleep(held);
        transaction.commit().expect("commit");
        released.recv().ok();
    });

    wait.recv().expect("the writer took the lock");
    let started = Instant::now();
    let cache = Cache::open(Some(&state.path)).expect("open the cache under a held write lock");
    let opened = started.elapsed();
    let list = cache
        .issue_list(&slug(SLUG), IssueStates::Open)
        .expect("read under a held write lock");
    let read = started.elapsed();

    eprintln!(
        "under a write lock held for {held:?}: opening the cache took {opened:?}, \
         opening and reading the list took {read:?}"
    );
    assert!(
        read < held / 3,
        "the reader waited {read:?} out of the {held:?} the lock was held for \
         (opening alone took {opened:?})"
    );
    assert_eq!(
        list.rows.len(),
        2,
        "and it read the committed list, not the half-written one"
    );

    release.send(()).ok();
    writer.join().expect("the writer finished");
    let after = Cache::open(Some(&state.path))
        .expect("reopen")
        .issue_list(&slug(SLUG), IssueStates::Open)
        .expect("the list after the commit");
    assert_eq!(after.rows.len(), 3, "and the write landed");
}

/// Two panes, two repos, one database, at the same time — each launching over
/// and over the way a workspace switch does, while a third reads throughout.
///
/// The reader is the assertion that matters: every write in the viewer is one
/// transaction, so a pane reading the list gets the whole of the old one or the
/// whole of the new one. A read that ever saw one row of a two-row list would
/// fail here.
#[test]
fn two_panes_against_one_database_both_read_and_write_correctly() {
    let mine = FixtureRepo::with_origin(REMOTE);
    let theirs = FixtureRepo::with_origin(OTHER_REMOTE);
    let state = StateDir::empty();
    let my_stub = stub();
    let their_stub = stub_for(
        OTHER_SLUG,
        vec![(3, "Something else entirely"), (4, "And another")],
    );
    seed(&mine.path, &my_stub, &state);
    seed(&theirs.path, &their_stub, &state);

    const LAUNCHES: usize = 12;
    let shared = &state;
    thread::scope(|scope| {
        for (workspace, stub) in [(&mine.path, &my_stub), (&theirs.path, &their_stub)] {
            scope.spawn(move || {
                for _ in 0..LAUNCHES {
                    let mut app = start(&pane(workspace, stub, shared));
                    press(&mut app, KeyCode::Enter);
                    assert!(screen(&app, 72, 16).contains("One column, drill-in."));
                }
            });
        }
        scope.spawn(move || {
            let cache = Cache::open(Some(&shared.path)).expect("the reading pane's cache");
            for _ in 0..(LAUNCHES * 20) {
                for repo in [SLUG, OTHER_SLUG] {
                    let list = cache
                        .issue_list(&slug(repo), IssueStates::Open)
                        .unwrap_or_else(|| panic!("{repo} was readable throughout"));
                    assert_eq!(
                        list.rows.len(),
                        2,
                        "{repo} was read mid-write: a list is one transaction or none"
                    );
                }
            }
        });
    });

    // Both panes' work is in the file, and neither displaced the other.
    let cache = Cache::open(Some(&state.path)).expect("reopen the database both wrote");
    for (repo, title) in [
        (SLUG, "Pane UI shape"),
        (OTHER_SLUG, "Something else entirely"),
    ] {
        let list = cache
            .issue_list(&slug(repo), IssueStates::Open)
            .unwrap_or_else(|| panic!("{repo} survived"));
        assert_eq!(list.rows.len(), 2);
        assert!(list.rows.iter().any(|row| row.title == title));
        assert!(
            cache
                .issue_detail(&slug(repo), list.rows[0].number)
                .is_some(),
            "{repo} kept the detail its pane read"
        );
    }
    let connection = database(&state);
    assert_eq!(
        scalar(
            &connection,
            "SELECT open_count FROM repo WHERE slug = ?1",
            SLUG
        ) as usize,
        LAUNCHES + 1,
        "every launch counted exactly once, races and all"
    );
}

/// Two panes launching at the same instant both prune. They serialise on the
/// write lock rather than one of them failing, and between them they take
/// exactly the aged repos.
#[test]
fn two_panes_pruning_one_database_at_once_agree_on_what_is_left() {
    let state = StateDir::empty();
    let path = state.database();
    seed_many(&path, 40, 4);
    let connection = database(&state);
    for index in 0..40 {
        // Every other repo has been abandoned for a hundred days.
        let aged = index % 2 == 0;
        age_back(
            &connection,
            &format!("owner/repo-{index}"),
            if aged { 100 } else { 1 },
            1,
        );
    }
    drop(connection);

    let taken: Vec<usize> = thread::scope(|scope| {
        let pruners: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    Cache::open_at(&path)
                        .expect("a pane's cache")
                        .prune(PrunePolicy::after_days(30, 90))
                        .repos
                })
            })
            .collect();
        pruners
            .into_iter()
            .map(|pruner| pruner.join().expect("the pruning pane finished"))
            .collect()
    });

    assert_eq!(
        taken.iter().sum::<usize>(),
        20,
        "between them they took each aged repo exactly once, and took it once only"
    );
    let connection = database(&state);
    let repos: i64 = connection
        .query_row("SELECT COUNT(*) FROM repo", [], |row| row.get(0))
        .expect("count the repos");
    assert_eq!(repos, 20);
    for index in 0..40 {
        let expected = if index % 2 == 0 { 0 } else { 4 };
        assert_eq!(
            count(&connection, "issue_list", &format!("owner/repo-{index}")),
            expected,
            "repo-{index}"
        );
    }
}

/// The prune sits on the startup path, so what it costs is the question, and
/// the answer has to be measured on more rows than a pane will ever have.
///
/// The bound is generous on purpose — this runs on whatever a CI box is — but
/// the number it prints is the point, and it is milliseconds on a database far
/// past anything SPEC §12's "a few MB of text" describes.
#[test]
fn the_prune_is_cheap_enough_to_sit_on_the_startup_path() {
    let state = StateDir::empty();
    let path = state.database();
    // 200 repos of 50 issues: 200 repo rows, 10,000 list rows, 10,000 details
    // and 10,000 comment pages.
    seed_many(&path, 200, 50);
    let connection = database(&state);
    for index in 0..200 {
        age_back(
            &connection,
            &format!("owner/repo-{index}"),
            if index % 2 == 0 { 100 } else { 1 },
            if index % 4 == 1 { 60 } else { 1 },
        );
    }
    drop(connection);

    let cache = Cache::open_at(&path).expect("open the seeded cache");
    let started = Instant::now();
    let pruned = cache.prune(PrunePolicy::after_days(30, 90));
    let elapsed = started.elapsed();

    assert_eq!(pruned.repos, 100);
    assert_eq!(pruned.list_rows, 5_000);
    assert_eq!(
        pruned.details,
        5_000 + 2_500,
        "the aged repos' details, plus the aged details of repos that stayed"
    );
    // And the case that actually happens: a launch with nothing old enough to
    // take, on a database of the same size.
    let started = Instant::now();
    let second = cache.prune(PrunePolicy::after_days(30, 90));
    let scan = started.elapsed();

    assert!(
        second.is_empty(),
        "the second launch has nothing left to do"
    );
    eprintln!(
        "prune of 200 repos / 10,000 issues took {elapsed:?} \
         ({} repos, {} list rows, {} details, {} comment pages deleted); \
         a launch with nothing to delete took {scan:?}",
        pruned.repos, pruned.list_rows, pruned.details, pruned.comment_pages
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "the prune took {elapsed:?}, which is not something to put in front of a keystroke"
    );
    assert!(
        scan < Duration::from_secs(1),
        "the empty prune took {scan:?}"
    );
}

/// A database with `repos` repos of `issues` issues each, written straight in.
///
/// Seeded with SQL rather than through panes because the point is the row
/// count: ten thousand issues is more than any pane will ever cache, and it is
/// the size the prune has to be cheap at.
fn seed_many(path: &PathBuf, repos: usize, issues: usize) {
    // Through the viewer's own opener, so the file has its schema and its
    // pragmas.
    drop(Cache::open_at(path).expect("create the database"));
    let mut connection = Connection::open(path).expect("seed the database");
    let now = herdr_issues::age::now();
    let transaction = connection.transaction().expect("one seeding transaction");
    for repo in 0..repos {
        let slug = format!("owner/repo-{repo}");
        transaction
            .execute(
                "INSERT INTO repo (slug, fetched_at, opened_at, open_count) VALUES (?1, ?2, ?2, 1)",
                params![slug, now],
            )
            .expect("insert the repo");
        for number in 0..issues as i64 {
            transaction
                .execute(
                    "INSERT INTO issue_list (slug, number, title, state, updated_at,
                                             comment_count, author, labels_json)
                     VALUES (?1, ?2, 'A title of about the usual length', 'OPEN',
                             '2026-07-27T09:14:03Z', 3, 'nyanyaon', '[\"prototype\"]')",
                    params![slug, number],
                )
                .expect("insert the list row");
            transaction
                .execute(
                    "INSERT INTO issue_detail (slug, number, body, updated_at, fetched_at,
                                               touched_at)
                     VALUES (?1, ?2, 'One column, drill-in.', '2026-07-27T09:14:03Z', ?3, ?3)",
                    params![slug, number, now],
                )
                .expect("insert the detail");
            transaction
                .execute(
                    "INSERT INTO issue_comments (slug, number, page, nodes_json, end_cursor,
                                                 has_next)
                     VALUES (?1, ?2, 1, '[]', NULL, 0)",
                    params![slug, number],
                )
                .expect("insert the comment page");
        }
    }
    transaction.commit().expect("commit the seed");
}
