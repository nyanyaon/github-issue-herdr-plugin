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

use std::collections::HashMap;
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

/// Seconds in a day, which is the unit both prune ages are configured in.
const SECONDS_PER_DAY: i64 = 86_400;

/// How old a row may get before the startup prune takes it, and how big the
/// file may get before compaction is worth its cost.
///
/// The two ages come from `config.toml` (SPEC §10); the threshold is fixed,
/// because it is a property of what a `VACUUM` costs rather than something a
/// user has an opinion about. It is a field rather than a constant only so that
/// a test can put a database above the threshold without writing 64 MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrunePolicy {
    /// `prune_details_after_days` — how long a detail survives untouched.
    pub details_after_days: u32,
    /// `prune_repos_after_days` — how long a repo survives unopened.
    pub repos_after_days: u32,
    /// The size past which a prune that freed pages is followed by a `VACUUM`.
    pub compact_above_bytes: u64,
}

impl PrunePolicy {
    /// SPEC §9: `VACUUM` only when the file exceeds ~64 MB.
    ///
    /// A `VACUUM` rewrites the whole database, so on the startup path it is
    /// only worth doing when there is a real amount of space to win back. Below
    /// this the freed pages are simply reused by the next write, which is what
    /// SQLite's freelist is for.
    pub const COMPACT_ABOVE_BYTES: u64 = 64 * 1024 * 1024;

    /// The policy for the configured ages, with the standard threshold.
    pub fn after_days(details_after_days: u32, repos_after_days: u32) -> Self {
        Self {
            details_after_days,
            repos_after_days,
            compact_above_bytes: Self::COMPACT_ABOVE_BYTES,
        }
    }
}

/// What one startup prune took, and whether it compacted afterwards.
///
/// Nothing renders this — the prune is silent, like every other thing the cache
/// does — but it is what the compaction threshold is decided from, and what the
/// tests assert "and nothing else" against.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    /// `repo` rows dropped for being unopened past their age.
    pub repos: usize,
    /// `issue_list` rows dropped, all of them belonging to a dropped repo:
    /// **the detail prune never touches the list**.
    pub list_rows: usize,
    /// `issue_detail` rows dropped, by either rule.
    pub details: usize,
    /// `issue_comments` pages dropped, always with the detail they belonged to.
    pub comment_pages: usize,
    /// Whether the file was over the threshold and got compacted.
    pub compacted: bool,
}

impl Pruned {
    /// Did this prune free anything at all? A prune that deleted nothing has
    /// left no free pages behind, so there is nothing for a `VACUUM` to win.
    pub fn is_empty(&self) -> bool {
        self.repos == 0 && self.list_rows == 0 && self.details == 0 && self.comment_pages == 0
    }
}

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
    ///
    /// Every comment page cached for the issue goes with the body it belonged
    /// to, and the thread starts again at page one (ADR-0001).
    pub fn save_issue_detail(&self, slug: &Slug, detail: &IssueDetail) {
        let _ = self.write_issue_detail(slug, detail);
    }

    /// The `updatedAt` each cached detail of this repo was fetched at, by issue
    /// number — the other half of the staleness comparison.
    ///
    /// A row is **stale** when the list's `updatedAt` differs from the one
    /// recorded here; an issue absent from this map has no cached detail at
    /// all, which is not the same thing as having a stale one. One query for
    /// the whole repo, because the marker is wanted for every row at once, and
    /// no clock is consulted anywhere in it.
    pub fn detail_updated_at(&self, slug: &Slug) -> HashMap<u64, Option<i64>> {
        self.read_detail_updated_at(slug).unwrap_or_default()
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

    /// The startup prune (SPEC §9), and the compaction that occasionally
    /// follows it.
    ///
    /// Two rules, and **nothing else**:
    ///
    /// - a repo nothing has opened for `repos_after_days` loses every row it
    ///   has, in all four tables;
    /// - a detail nothing has displayed for `details_after_days` loses its body
    ///   and its comment pages — **and not its list row**, which is a row of the
    ///   repo's list rather than of the detail, costs a few dozen bytes, and is
    ///   what makes the pane's next frame instant.
    ///
    /// Both ages are read off columns GitHub had no part in: `repo.opened_at`
    /// and `issue_detail.touched_at` are written when a pane displays the thing
    /// they belong to. This is the one place in the viewer where a clock decides
    /// anything, and it decides only what to *forget* — never what is
    /// **stale**, which stays a disagreement between two `updatedAt`s.
    ///
    /// One transaction, so another pane reads the database either before this
    /// prune or after it. `IMMEDIATE`, so two panes pruning at once serialise on
    /// the write lock rather than one of them failing to upgrade halfway
    /// through.
    ///
    /// Silent on failure, like everything else here: a prune that could not run
    /// leaves a slightly larger cache and nothing worse.
    pub fn prune(&self, policy: PrunePolicy) -> Pruned {
        let mut pruned = self.delete_aged_rows(policy).unwrap_or_default();
        if !pruned.is_empty() {
            pruned.compacted = self
                .compact_above(policy.compact_above_bytes)
                .unwrap_or(false);
        }
        pruned
    }

    fn delete_aged_rows(&self, policy: PrunePolicy) -> rusqlite::Result<Pruned> {
        let now = age::now();
        let repos_before = now - days(policy.repos_after_days);
        let details_before = now - days(policy.details_after_days);
        let mut pruned = Pruned::default();

        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;

        // Every row of a repo nothing has opened in long enough. The child rows
        // go first, so that a failure anywhere cannot leave a `repo` row without
        // its list — though the transaction would have rolled that back anyway.
        const OF_A_STALE_REPO: &str = "slug IN (SELECT slug FROM repo WHERE opened_at < ?1)";
        for (table, counter) in [
            ("issue_comments", &mut pruned.comment_pages),
            ("issue_detail", &mut pruned.details),
            ("issue_list", &mut pruned.list_rows),
        ] {
            *counter = transaction.execute(
                &format!("DELETE FROM {table} WHERE {OF_A_STALE_REPO}"),
                params![repos_before],
            )?;
        }
        pruned.repos = transaction.execute(
            "DELETE FROM repo WHERE opened_at < ?1",
            params![repos_before],
        )?;

        // Then the details of repos that are staying: the pages first, while
        // the rows naming them are still there to be joined against.
        pruned.comment_pages += transaction.execute(
            "DELETE FROM issue_comments
             WHERE EXISTS (SELECT 1 FROM issue_detail d
                           WHERE d.slug = issue_comments.slug
                             AND d.number = issue_comments.number
                             AND d.touched_at < ?1)",
            params![details_before],
        )?;
        pruned.details += transaction.execute(
            "DELETE FROM issue_detail WHERE touched_at < ?1",
            params![details_before],
        )?;

        transaction.commit()?;
        Ok(pruned)
    }

    /// `VACUUM`, but only when the database is bigger than `threshold`.
    ///
    /// The size is the main database file's — `page_count * page_size` — which
    /// is exactly the thing a `VACUUM` would shrink. Deleting rows does not
    /// shrink it: the pages go on the freelist, and stay part of the file until
    /// something rewrites it or reuses them.
    fn compact_above(&self, threshold: u64) -> rusqlite::Result<bool> {
        let pages: i64 = self
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        if pages.saturating_mul(page_size).max(0) as u64 <= threshold {
            return Ok(false);
        }
        self.connection.execute_batch("VACUUM")?;
        Ok(true)
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

    fn read_detail_updated_at(&self, slug: &Slug) -> rusqlite::Result<HashMap<u64, Option<i64>>> {
        let mut statement = self
            .connection
            .prepare("SELECT number, updated_at FROM issue_detail WHERE slug = ?1")?;
        let ages = statement.query_map(params![slug.to_string()], |row| {
            let updated_at: Option<String> = row.get(1)?;
            Ok((
                row.get::<_, i64>(0)? as u64,
                updated_at.as_deref().and_then(age::parse_timestamp),
            ))
        })?;
        ages.collect()
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

/// A configured age in the unix seconds the columns it is compared against are
/// kept in.
fn days(count: u32) -> i64 {
    i64::from(count) * SECONDS_PER_DAY
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

    /// Re-fetching an issue drops **every** page cached for it, not just the
    /// one it replaces, and starts the thread again at page one (ADR-0001).
    ///
    /// Only the first page is ever fetched so far, so the second page here is
    /// written straight into the table — the same standing this file's
    /// migration test has, which asserts against a schema step that does not
    /// exist yet either.
    #[test]
    fn a_re_fetch_drops_every_comment_page_and_starts_again_at_the_first() {
        let cache = Cache::open_at(&temp_database("pages")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());
        cache.save_issue_detail(&slug(), &detail_with_comments());
        cache
            .connection
            .execute(
                "INSERT INTO issue_comments (slug, number, page, nodes_json, end_cursor, has_next)
                 VALUES (?1, 7, 2, ?2, 'Y3Vyc29yOnYyOpHOBB', 0)",
                params![
                    slug().to_string(),
                    r#"[{"author":"octocat","created_at":null,"body":"The hundred and first."}]"#,
                ],
            )
            .expect("cache a second page the way paging would");
        assert_eq!(
            cache
                .issue_detail(&slug(), 7)
                .expect("the two-page thread")
                .comments
                .len(),
            2
        );

        // The issue moved on, so the pane fetched it again.
        let mut re_fetched = detail_with_comments();
        re_fetched.body = "One column, drill-in. Now with a marker.".to_string();
        re_fetched.comments[0].body = "The first of many, edited.".to_string();
        re_fetched.has_more_comments = false;
        re_fetched.comments_end_cursor = None;
        cache.save_issue_detail(&slug(), &re_fetched);

        let pages: Vec<i64> = cache
            .connection
            .prepare("SELECT page FROM issue_comments WHERE slug = ?1 AND number = 7 ORDER BY page")
            .expect("read the cached pages")
            .query_map(params![slug().to_string()], |row| row.get(0))
            .expect("read the cached pages")
            .collect::<rusqlite::Result<Vec<i64>>>()
            .expect("read the cached pages");
        assert_eq!(
            pages,
            vec![FIRST_COMMENT_PAGE],
            "the thread restarts at the first page"
        );

        let read = cache
            .issue_detail(&slug(), 7)
            .expect("the re-fetched detail");
        assert_eq!(read.comments.len(), 1);
        assert_eq!(read.comments[0].body, "The first of many, edited.");
        assert!(!read.has_more_comments);
        assert_eq!(read.comments_end_cursor, None);
    }

    /// The other half of the staleness comparison, straight off the table it is
    /// read from. An issue with no cached detail is simply absent — there is
    /// nothing behind the list for it to be.
    #[test]
    fn cached_detail_ages_are_reported_per_issue() {
        let cache = Cache::open_at(&temp_database("ages")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());
        cache.save_issue_detail(&slug(), &detail_with_comments());

        let ages = cache.detail_updated_at(&slug());

        assert_eq!(
            ages.get(&7).copied(),
            Some(age::parse_timestamp("2026-07-27T09:14:03Z"))
        );
        assert_eq!(ages.get(&8), None, "an issue never read has no cached age");
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

    /// Dates a repo and one of its details back, the way a database left alone
    /// for months would be. Both columns are written by the viewer, so this is
    /// the only way to make an old one.
    fn age_rows(cache: &Cache, slug: &Slug, repo_days: i64, detail_days: i64) {
        let then = |days: i64| age::now() - days * SECONDS_PER_DAY;
        cache
            .connection
            .execute(
                "UPDATE repo SET opened_at = ?2 WHERE slug = ?1",
                params![slug.to_string(), then(repo_days)],
            )
            .expect("date the repo back");
        cache
            .connection
            .execute(
                "UPDATE issue_detail SET touched_at = ?2 WHERE slug = ?1",
                params![slug.to_string(), then(detail_days)],
            )
            .expect("date the detail back");
    }

    fn count(cache: &Cache, table: &str, slug: &Slug) -> i64 {
        cache
            .connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE slug = ?1"),
                params![slug.to_string()],
                |row| row.get(0),
            )
            .expect("count the rows")
    }

    /// The prune's first rule, and the half of it that is easy to get wrong: a
    /// detail nothing has displayed for long enough loses its body and its
    /// comment pages — **and nothing else**. Its list row stays, because the row
    /// belongs to the repo's list rather than to the detail, and it is what the
    /// next pane draws its first frame from.
    #[test]
    fn an_aged_detail_takes_its_comment_pages_and_leaves_its_list_row() {
        let cache = Cache::open_at(&temp_database("aged-detail")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());
        cache.save_issue_detail(&slug(), &detail_with_comments());
        age_rows(&cache, &slug(), 1, 31);

        let pruned = cache.prune(PrunePolicy::after_days(30, 90));

        assert_eq!(pruned.details, 1);
        assert_eq!(pruned.comment_pages, 1);
        assert_eq!(pruned.repos, 0, "the repo was opened yesterday");
        assert_eq!(pruned.list_rows, 0, "the list is not the detail's to take");
        assert_eq!(count(&cache, "issue_detail", &slug()), 0);
        assert_eq!(count(&cache, "issue_comments", &slug()), 0);
        assert_eq!(count(&cache, "issue_list", &slug()), 1);
        assert_eq!(count(&cache, "repo", &slug()), 1);
        assert_eq!(
            cache
                .issue_list(&slug(), IssueStates::Open)
                .expect("the rows a warm start draws")
                .rows
                .len(),
            1
        );
    }

    /// The other side of the same rule: a detail read the day before yesterday
    /// is not old, and a prune that deleted it would be deleting the thing the
    /// cache exists for.
    #[test]
    fn a_detail_inside_its_age_is_left_alone() {
        let cache = Cache::open_at(&temp_database("fresh-detail")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());
        cache.save_issue_detail(&slug(), &detail_with_comments());
        age_rows(&cache, &slug(), 1, 29);

        let pruned = cache.prune(PrunePolicy::after_days(30, 90));

        assert_eq!(pruned, Pruned::default(), "a prune with nothing to take");
        assert!(pruned.is_empty());
        assert_eq!(
            cache
                .issue_detail(&slug(), 7)
                .expect("the detail survived")
                .comments
                .len(),
            1,
            "and so did its comment page"
        );
    }

    /// The prune's second rule spans every table: a repo nothing has opened in
    /// long enough leaves nothing behind — and takes nothing of another repo
    /// with it.
    #[test]
    fn an_unopened_repo_loses_every_row_it_has_and_only_its_own() {
        let cache = Cache::open_at(&temp_database("aged-repo")).expect("open the cache");
        let abandoned = Slug::parse("octocat/abandoned").expect("a slug this test wrote");
        for repo in [&slug(), &abandoned] {
            cache.save_issue_list(repo, &list_with_one_row());
            cache.save_issue_detail(repo, &detail_with_comments());
        }
        // Unopened for a hundred days, but read only yesterday: the repo rule
        // takes it anyway, detail age and all.
        age_rows(&cache, &abandoned, 100, 1);
        age_rows(&cache, &slug(), 1, 1);

        let pruned = cache.prune(PrunePolicy::after_days(30, 90));

        assert_eq!(pruned.repos, 1);
        assert_eq!(pruned.list_rows, 1);
        assert_eq!(pruned.details, 1);
        assert_eq!(pruned.comment_pages, 1);
        for table in ["repo", "issue_list", "issue_detail", "issue_comments"] {
            assert_eq!(count(&cache, table, &abandoned), 0, "{table} kept a row");
            assert_eq!(count(&cache, table, &slug()), 1, "{table} lost a row");
        }
        assert!(
            cache.issue_list(&abandoned, IssueStates::Open).is_none(),
            "the abandoned repo is a cold start again"
        );
        assert!(cache.issue_detail(&slug(), 7).is_some());
    }

    /// A prune that took nothing left no free pages behind, so there is nothing
    /// to compact however big the file is.
    #[test]
    fn a_prune_that_deletes_nothing_never_compacts() {
        let cache = Cache::open_at(&temp_database("no-compaction")).expect("open the cache");
        cache.save_issue_list(&slug(), &list_with_one_row());

        let pruned = cache.prune(PrunePolicy {
            compact_above_bytes: 0,
            ..PrunePolicy::after_days(30, 90)
        });

        assert!(pruned.is_empty());
        assert!(!pruned.compacted);
    }

    /// Compaction is the size threshold's call, not the launch's (SPEC §9).
    ///
    /// A test database is a few pages, so the standard 64 MB threshold is the
    /// realistic case — every launch, for years — and it must not `VACUUM`. The
    /// threshold is then dropped to prove the other branch really is there, and
    /// that it is a `VACUUM`: the freelist the deletes left behind is gone
    /// afterwards.
    #[test]
    fn compaction_waits_for_the_size_threshold_rather_than_the_launch() {
        let path = temp_database("compaction");
        let seed = |cache: &Cache| {
            cache.save_issue_list(&slug(), &list_with_one_row());
            cache.save_issue_detail(&slug(), &detail_with_comments());
            age_rows(cache, &slug(), 1, 31);
        };

        let cache = Cache::open_at(&path).expect("open the cache");
        seed(&cache);
        let pruned = cache.prune(PrunePolicy::after_days(30, 90));
        assert!(!pruned.is_empty(), "there was something to delete");
        assert!(
            !pruned.compacted,
            "a few kilobytes is not worth rewriting the file for"
        );

        seed(&cache);
        let freelist = |cache: &Cache| -> i64 {
            cache
                .connection
                .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                .expect("read the freelist")
        };
        let pruned = cache.prune(PrunePolicy {
            compact_above_bytes: 0,
            ..PrunePolicy::after_days(30, 90)
        });

        assert!(pruned.compacted, "past the threshold it does compact");
        assert_eq!(freelist(&cache), 0, "the freed pages left the file");
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
