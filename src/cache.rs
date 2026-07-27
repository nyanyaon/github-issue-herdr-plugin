//! The cache: one SQLite database, shared by every pane (ADR-0001).
//!
//! It is read *before* the network and written through as answers arrive, which
//! is what makes opening a pane on a repo already read instant. One database at
//! `$HERDR_PLUGIN_STATE_DIR/cache.sqlite3` serves every pane and every herdr
//! session, in WAL mode so that a refresh in one pane never blocks a read in
//! another.
//!
//! Nothing here is allowed to take the pane down. The data lives on GitHub; a
//! database that cannot be opened, read or written is a cache miss and nothing
//! more, so every entry point answers `Option` or `()` and swallows the reason.
//!
//! The schema is SPEC §9's, verbatim, carried forward by `user_version`
//! migrations: a version bump migrates rather than wipes, because an old pane
//! and a new one may share this file during an upgrade.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::age;
use crate::github::{IssueComment, IssueDetail, IssueList, IssueRow, IssueStates};
use crate::identity::Slug;

/// The one database, in the state dir. Named by the plugin id through that
/// directory, so two plugins never share a file and ten panes always do.
pub const FILE_NAME: &str = "cache.sqlite3";

/// The first page of an issue's comments. Pages are cached individually so a
/// long thread can be resumed from its cursor.
const FIRST_COMMENT_PAGE: i64 = 1;

/// The schema, exactly as SPEC §9 states it.
const SCHEMA_V1: &str = "
CREATE TABLE repo (
  slug TEXT PRIMARY KEY,            -- the resolved identity slug, not nameWithOwner:
                                    -- it is the only key available before the fetch
  fetched_at INTEGER NOT NULL,      -- unix seconds, last successful list query
  opened_at  INTEGER NOT NULL,      -- last time a pane displayed this repo
  open_count INTEGER
);
CREATE TABLE issue_list (           -- one row per issue from the list query
  slug TEXT NOT NULL, number INTEGER NOT NULL,
  title TEXT, state TEXT, updated_at TEXT, comment_count INTEGER,
  author TEXT, labels_json TEXT,
  PRIMARY KEY (slug, number)
);
CREATE TABLE issue_detail (
  slug TEXT NOT NULL, number INTEGER NOT NULL,
  body TEXT, updated_at TEXT,       -- the updatedAt this detail was fetched at
  fetched_at INTEGER NOT NULL, touched_at INTEGER NOT NULL,
  PRIMARY KEY (slug, number)
);
CREATE TABLE issue_comments (       -- one row per fetched page
  slug TEXT NOT NULL, number INTEGER NOT NULL, page INTEGER NOT NULL,
  nodes_json TEXT NOT NULL, end_cursor TEXT, has_next INTEGER NOT NULL,
  PRIMARY KEY (slug, number, page)
);
";

/// Every schema step, in order. A step's index plus one is the `user_version` it
/// leaves behind, so adding a version is adding an entry and nothing else.
///
/// Two rules keep a shared file safe across an upgrade, and both are why steps
/// are additive statements rather than a fresh `CREATE`:
///
/// - A database older than this build is carried forward by the steps it is
///   missing. **Data is migrated, never dropped.**
/// - A database *newer* than this build is left exactly as it is. An older pane
///   keeps reading the columns it knows, because no step ever removes one.
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

pub struct Cache {
    connection: Connection,
}

impl Cache {
    /// Opens the shared database under the state dir, creating it on the first
    /// run.
    ///
    /// `None` when there is no state dir — the viewer running outside herdr,
    /// which hands out no such directory — or when the file cannot be opened at
    /// all. Either way the pane works, and simply fetches every time.
    pub fn open(state_dir: Option<&Path>) -> Option<Self> {
        let directory = state_dir?;
        std::fs::create_dir_all(directory).ok()?;
        Self::open_at(&directory.join(FILE_NAME)).ok()
    }

    /// Opens one database file, with the pragmas and the migrations SPEC §9
    /// asks for.
    pub fn open_at(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        // WAL so readers in other panes never block on a refresh in this one,
        // and a busy timeout so the writes that *do* serialise wait rather than
        // fail.
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=3000;")?;
        migrate(&connection, MIGRATIONS)?;
        Ok(Self { connection })
    }

    /// The schema version this file is at.
    pub fn schema_version(&self) -> i64 {
        user_version(&self.connection).unwrap_or_default()
    }

    /// The cached issue list for a repo, in the same order the query returns:
    /// most recently updated first.
    ///
    /// Rows are filtered by the state being displayed, because the cache holds
    /// whatever the last query asked for and a pane that opens on `open` must
    /// not show yesterday's closed issues as if they were open.
    pub fn issue_list(&self, slug: &Slug, states: IssueStates) -> Option<IssueList> {
        self.read_issue_list(slug, states).ok().flatten()
    }

    /// Writes a fetched list through, as it arrives.
    ///
    /// The rows are replaced rather than merged: the cache holds what the last
    /// query answered, so an issue that has left the list leaves the cache with
    /// it. One transaction, so another pane reads either the old list or the new
    /// one and never half of each.
    pub fn save_issue_list(&self, slug: &Slug, list: &IssueList) {
        let _ = self.write_issue_list(slug, list);
    }

    /// The cached detail for one issue: its body and comment pages, joined to
    /// the list row that holds its title, state, author and labels.
    ///
    /// `None` when either half is missing — a body with no list row has no
    /// title to render.
    pub fn issue_detail(&self, slug: &Slug, number: u64) -> Option<IssueDetail> {
        self.read_issue_detail(slug, number).ok().flatten()
    }

    /// Writes a fetched detail through, with its comment page.
    pub fn save_issue_detail(&self, slug: &Slug, detail: &IssueDetail) {
        let _ = self.write_issue_detail(slug, detail);
    }

    /// Records that a pane displayed this repo, which is what the startup prune
    /// ages repos by.
    ///
    /// Only ever an update: a repo nothing has been fetched for yet has no row
    /// to age, and gets one from the first list that arrives.
    pub fn mark_opened(&self, slug: &Slug) {
        let _ = self.connection.execute(
            "UPDATE repo SET opened_at = ?2, open_count = COALESCE(open_count, 0) + 1
             WHERE slug = ?1",
            params![slug.to_string(), age::now()],
        );
    }

    fn read_issue_list(
        &self,
        slug: &Slug,
        states: IssueStates,
    ) -> rusqlite::Result<Option<IssueList>> {
        let slug = slug.to_string();
        let fetched_at: Option<i64> = self
            .connection
            .query_row(
                "SELECT fetched_at FROM repo WHERE slug = ?1",
                params![slug],
                |row| row.get(0),
            )
            .optional()?;
        let Some(fetched_at) = fetched_at else {
            return Ok(None);
        };

        let mut statement = self.connection.prepare(
            "SELECT number, title, state, updated_at, comment_count, author, labels_json
             FROM issue_list
             WHERE slug = ?1 AND (?2 IS NULL OR state = ?2)
             ORDER BY updated_at DESC",
        )?;
        let rows = statement
            .query_map(params![slug, state_filter(states)], |row| {
                let updated_at: Option<String> = row.get(3)?;
                let labels: Option<String> = row.get(6)?;
                Ok(IssueRow {
                    number: row.get::<_, i64>(0)? as u64,
                    title: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    state: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    updated_at: updated_at.as_deref().and_then(age::parse_timestamp),
                    comment_count: row.get::<_, Option<i64>>(4)?.unwrap_or_default() as u64,
                    author: row.get(5)?,
                    labels: labels
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<IssueRow>>>()?;

        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(IssueList {
            // The name the API answered with last time, which is what the repo
            // is keyed by and what the header shows until this fetch replaces
            // it.
            name_with_owner: slug,
            // What is cached is all the count can mean here; the fetch that
            // follows replaces it with what the repo actually holds.
            total_count: rows.len() as u64,
            rows,
            fetched_at,
        }))
    }

    fn write_issue_list(&self, slug: &Slug, list: &IssueList) -> rusqlite::Result<()> {
        let slug = slug.to_string();
        let now = age::now();
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO repo (slug, fetched_at, opened_at, open_count) VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(slug) DO UPDATE SET fetched_at = excluded.fetched_at,
                                             opened_at = excluded.opened_at",
            params![slug, list.fetched_at, now],
        )?;
        transaction.execute("DELETE FROM issue_list WHERE slug = ?1", params![slug])?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO issue_list
                   (slug, number, title, state, updated_at, comment_count, author, labels_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for row in &list.rows {
                insert.execute(params![
                    slug,
                    row.number as i64,
                    row.title,
                    row.state,
                    row.updated_at.map(age::format_timestamp),
                    row.comment_count as i64,
                    row.author,
                    serde_json::to_string(&row.labels).unwrap_or_else(|_| "[]".to_string()),
                ])?;
            }
        }
        transaction.commit()
    }

    fn read_issue_detail(&self, slug: &Slug, number: u64) -> rusqlite::Result<Option<IssueDetail>> {
        let slug_text = slug.to_string();
        let detail = self
            .connection
            .query_row(
                "SELECT d.body, d.fetched_at, l.title, l.state, l.updated_at, l.comment_count,
                        l.author, l.labels_json
                 FROM issue_detail d
                 JOIN issue_list l ON l.slug = d.slug AND l.number = d.number
                 WHERE d.slug = ?1 AND d.number = ?2",
                params![slug_text, number as i64],
                |row| {
                    let updated_at: Option<String> = row.get(4)?;
                    let labels: Option<String> = row.get(7)?;
                    Ok(IssueDetail {
                        number,
                        title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        body: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        state: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        updated_at: updated_at.as_deref().and_then(age::parse_timestamp),
                        author: row.get(6)?,
                        labels: labels
                            .and_then(|json| serde_json::from_str(&json).ok())
                            .unwrap_or_default(),
                        comment_total_count: row.get::<_, Option<i64>>(5)?.unwrap_or_default()
                            as u64,
                        comments: Vec::new(),
                        has_more_comments: false,
                        comments_end_cursor: None,
                        fetched_at: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let Some(mut detail) = detail else {
            return Ok(None);
        };

        // Pages in the order they were fetched, so the thread reads as one
        // conversation however many pages it took to gather.
        let mut statement = self.connection.prepare(
            "SELECT nodes_json, end_cursor, has_next FROM issue_comments
             WHERE slug = ?1 AND number = ?2 ORDER BY page",
        )?;
        let pages = statement
            .query_map(params![slug_text, number as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (nodes_json, end_cursor, has_next) in pages {
            let nodes: Vec<IssueComment> = serde_json::from_str(&nodes_json).unwrap_or_default();
            detail.comments.extend(nodes);
            detail.comments_end_cursor = end_cursor;
            detail.has_more_comments = has_next;
        }

        // Displaying a detail is what keeps it from being pruned.
        let _ = self.connection.execute(
            "UPDATE issue_detail SET touched_at = ?3 WHERE slug = ?1 AND number = ?2",
            params![slug_text, number as i64, age::now()],
        );
        Ok(Some(detail))
    }

    fn write_issue_detail(&self, slug: &Slug, detail: &IssueDetail) -> rusqlite::Result<()> {
        let slug_text = slug.to_string();
        let number = detail.number as i64;
        let now = age::now();
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO issue_detail (slug, number, body, updated_at, fetched_at, touched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(slug, number) DO UPDATE SET body = excluded.body,
                                                     updated_at = excluded.updated_at,
                                                     fetched_at = excluded.fetched_at,
                                                     touched_at = excluded.touched_at",
            params![
                slug_text,
                number,
                detail.body,
                detail.updated_at.map(age::format_timestamp),
                detail.fetched_at,
                now,
            ],
        )?;
        // A fetched detail starts the thread again at page one, so whatever
        // pages were cached for it are gone with the body they belonged to.
        transaction.execute(
            "DELETE FROM issue_comments WHERE slug = ?1 AND number = ?2",
            params![slug_text, number],
        )?;
        transaction.execute(
            "INSERT INTO issue_comments (slug, number, page, nodes_json, end_cursor, has_next)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                slug_text,
                number,
                FIRST_COMMENT_PAGE,
                serde_json::to_string(&detail.comments).unwrap_or_else(|_| "[]".to_string()),
                detail.comments_end_cursor,
                detail.has_more_comments as i64,
            ],
        )?;
        transaction.commit()
    }
}

/// The `state` column a set of states matches, or `NULL` for all of them.
fn state_filter(states: IssueStates) -> Option<&'static str> {
    match states {
        IssueStates::Open => Some("OPEN"),
        IssueStates::Closed => Some("CLOSED"),
        IssueStates::All => None,
    }
}

fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Carries a database forward to the last of `migrations`, applying only the
/// steps it is missing.
///
/// The whole thing is one `IMMEDIATE` transaction, which is what makes two panes
/// opening the same fresh file at once safe: the second waits out the first
/// (that is what the busy timeout is for), then re-reads `user_version` inside
/// the transaction and finds there is nothing left to do. `user_version` is
/// stored in the database header and rolls back with everything else, so a step
/// that fails leaves the file at the version it was already at.
fn migrate(connection: &Connection, migrations: &[&str]) -> rusqlite::Result<()> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    let current = user_version(&transaction)?;
    for (index, statements) in migrations.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        transaction.execute_batch(statements)?;
        transaction.pragma_update(None, "user_version", version)?;
    }
    transaction.commit()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn temp_database(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let directory = std::env::temp_dir().join(format!(
            "herdr-issues-cache-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).expect("create the temp directory");
        directory.join(FILE_NAME)
    }

    fn slug() -> Slug {
        Slug::parse("nyanyaon/github-issue-herdr-plugin").expect("a slug this test wrote")
    }

    fn detail_with_comments() -> IssueDetail {
        IssueDetail {
            number: 7,
            title: "Pane UI shape".to_string(),
            body: "One column, drill-in.".to_string(),
            state: "OPEN".to_string(),
            updated_at: age::parse_timestamp("2026-07-27T09:14:03Z"),
            author: Some("nyanyaon".to_string()),
            labels: vec!["prototype".to_string()],
            comment_total_count: 107,
            comments: vec![IssueComment {
                author: Some("nyanyaon".to_string()),
                created_at: age::parse_timestamp("2026-07-26T09:14:03Z"),
                body: "The first of many.".to_string(),
            }],
            has_more_comments: true,
            comments_end_cursor: Some("Y3Vyc29yOnYyOpHOAA".to_string()),
            fetched_at: 1_785_143_643,
        }
    }

    fn list_with_one_row() -> IssueList {
        IssueList {
            name_with_owner: "nyanyaon/github-issue-herdr-plugin".to_string(),
            total_count: 1,
            rows: vec![IssueRow {
                number: 7,
                title: "Pane UI shape".to_string(),
                state: "OPEN".to_string(),
                updated_at: age::parse_timestamp("2026-07-27T09:14:03Z"),
                comment_count: 107,
                author: Some("nyanyaon".to_string()),
                labels: vec!["prototype".to_string()],
            }],
            fetched_at: 1_785_143_600,
        }
    }

    #[test]
    fn the_database_is_in_wal_mode_with_a_busy_timeout() {
        let cache = Cache::open_at(&temp_database("pragmas")).expect("open the cache");

        let journal_mode: String = cache
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read the journal mode");
        let busy_timeout: i64 = cache
            .connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("read the busy timeout");

        assert_eq!(journal_mode, "wal");
        assert_eq!(busy_timeout, 3_000);
        assert_eq!(cache.schema_version(), MIGRATIONS.len() as i64);
    }

    /// The comment page is written with the cursor and the has-next flag the
    /// schema keeps columns for. Nothing pages yet, so this is the only place
    /// either is visible.
    #[test]
    fn a_detail_round_trips_with_its_comment_page_cursor() {
        let cache = Cache::open_at(&temp_database("detail")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());
        cache.save_issue_detail(&slug(), &detail_with_comments());

        let read = cache
            .issue_detail(&slug(), 7)
            .expect("the detail just written");

        assert_eq!(read.body, "One column, drill-in.");
        // Title, state, author and labels come from the list row: the detail
        // table holds the body and nothing the list already carries.
        assert_eq!(read.title, "Pane UI shape");
        assert_eq!(read.author.as_deref(), Some("nyanyaon"));
        assert_eq!(read.labels, vec!["prototype".to_string()]);
        assert_eq!(read.comment_total_count, 107);
        assert_eq!(read.comments.len(), 1);
        assert_eq!(read.comments[0].body, "The first of many.");
        assert!(read.has_more_comments);
        assert_eq!(
            read.comments_end_cursor.as_deref(),
            Some("Y3Vyc29yOnYyOpHOAA")
        );
        assert_eq!(read.fetched_at, 1_785_143_643);
    }

    /// The criterion that is easy to claim and hard to have got right: a file
    /// written by an older build is carried forward, with its rows.
    ///
    /// The step here stands in for the next real one — there is only one version
    /// so far — and it goes through the same [`migrate`] the viewer runs.
    #[test]
    fn a_schema_version_bump_migrates_rather_than_wipes() {
        let path = temp_database("migration");
        {
            let cache = Cache::open_at(&path).expect("open the cache");
            cache.save_issue_list(&slug(), &list_with_one_row());
            cache.save_issue_detail(&slug(), &detail_with_comments());
            assert_eq!(cache.schema_version(), 1);
        }

        // A later build, one schema version further on.
        let next = "ALTER TABLE repo ADD COLUMN last_error TEXT;";
        let connection = Connection::open(&path).expect("reopen the database");
        migrate(&connection, &[SCHEMA_V1, next]).expect("apply the newer schema");

        assert_eq!(user_version(&connection).expect("the version"), 2);
        let cache = Cache { connection };
        let list = cache
            .issue_list(&slug(), IssueStates::Open)
            .expect("the list survived the bump");
        assert_eq!(list.rows.len(), 1);
        assert_eq!(list.rows[0].title, "Pane UI shape");
        assert_eq!(list.fetched_at, 1_785_143_600);
        assert_eq!(
            cache
                .issue_detail(&slug(), 7)
                .expect("the detail survived the bump")
                .body,
            "One column, drill-in."
        );

        // And the step really did run rather than being skipped.
        let added: i64 = cache
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('repo') WHERE name = 'last_error'",
                [],
                |row| row.get(0),
            )
            .expect("read the repo columns");
        assert_eq!(added, 1);

        // Re-running the same steps is a no-op rather than a second attempt.
        migrate(&cache.connection, &[SCHEMA_V1, next]).expect("re-run the migrations");
        assert_eq!(cache.schema_version(), 2);
    }

    /// A pane built before the bump still reads a file written after it: no step
    /// removes a column, so every column it knows is still there.
    #[test]
    fn a_newer_database_is_left_alone_by_an_older_pane() {
        let path = temp_database("newer");
        let connection = Connection::open(&path).expect("open the database");
        migrate(
            &connection,
            &[SCHEMA_V1, "ALTER TABLE repo ADD COLUMN last_error TEXT;"],
        )
        .expect("write a newer schema");
        {
            let cache = Cache { connection };
            cache.save_issue_list(&slug(), &list_with_one_row());
        }

        let cache = Cache::open_at(&path).expect("open the newer file with today's build");

        assert_eq!(cache.schema_version(), 2, "the version is not walked back");
        assert_eq!(
            cache
                .issue_list(&slug(), IssueStates::Open)
                .expect("the list is still readable")
                .rows
                .len(),
            1
        );
    }

    /// The cache holds what the last query answered, so a pane opening on `open`
    /// never shows cached closed issues as open ones.
    #[test]
    fn cached_rows_are_read_back_for_the_state_being_displayed() {
        let cache = Cache::open_at(&temp_database("states")).expect("open the cache");
        let mut list = list_with_one_row();
        list.rows[0].state = "CLOSED".to_string();
        cache.save_issue_list(&slug(), &list);

        assert!(cache.issue_list(&slug(), IssueStates::Open).is_none());
        assert_eq!(
            cache
                .issue_list(&slug(), IssueStates::Closed)
                .expect("the closed rows")
                .rows
                .len(),
            1
        );
        assert_eq!(
            cache
                .issue_list(&slug(), IssueStates::All)
                .expect("every cached row")
                .rows
                .len(),
            1
        );
    }
}
