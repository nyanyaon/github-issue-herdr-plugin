# ADR-0001: Issue cache is one shared SQLite database, invalidated by `updatedAt`

- **Status**: accepted
- **Date**: 2026-07-27
- **Ticket**: [Cache and refresh policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/6)

## Context

The pane exists because reading issues in a browser costs too much on a laptop already running several agents. Fetch-on-open plus manual refresh with a disk cache was fixed while charting the map. [`docs/research/github-access-from-rust.md`](../research/github-access-from-rust.md) then established that the GraphQL list query costs **4 KB wire / 16 KB parse** for 50 issues and already carries per-issue `updatedAt`, and [`docs/research/herdr-plugin-contract.md`](../research/herdr-plugin-contract.md) established that runtime state belongs in `HERDR_PLUGIN_STATE_DIR` and never in the plugin root.

Several panes run at once, across workspaces and across concurrent herdr sessions, and more than one may be pointed at the same repo.

## Decision

**Storage.** One SQLite database, `$HERDR_PLUGIN_STATE_DIR/cache.sqlite3`, shared by every pane, in **WAL** mode with a `busy_timeout`. Tables are keyed by repo. WAL gives concurrent readers alongside a single writer, so readers never block on a refresh happening in another pane, and ten panes share one file and one page cache instead of ten JSON blobs.

**Invalidation is `updatedAt`-driven, never timed.** On pane open and on every manual refresh, the pane re-runs the cheap list query. An issue whose `updatedAt` is newer than the cached detail's is marked stale; the detail is re-fetched when the issue is opened, not eagerly. Nothing expires on a clock — GitHub states what changed, so no TTL has to be guessed.

**Refresh granularity follows the view.** Refresh in the list view re-runs the list query; refresh in a detail view re-fetches that issue's body and comments. The exact keys are [Pane UI shape](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/7)'s call.

**Age is always visible.** The pane header always states the data's age — `cli/cli · 47 open · fetched 3m ago` — and list rows whose `updatedAt` has moved past the cached detail carry a marker. No threshold to pick, no hidden staleness.

**Offline serves the cache, marked.** A failed fetch never clears or blocks anything: the pane opens on cached data and the header says so (`fetched 4h ago · offline`). A refresh that fails reports the error and leaves the cache intact.

**Eviction is an age-based prune at startup.** On pane start, delete issue details untouched for 30 days and every repo not opened in 90 days; `VACUUM` only when the file exceeds a size threshold. One cheap `DELETE` per launch, no per-read bookkeeping.

## Consequences

- **`rusqlite` with `bundled` compiles SQLite from C**, so a published plugin either demands a C compiler on the installing user's machine or ships prebuilt binaries. This is now a hard constraint on [Packaging and distribution for a publishable Rust plugin](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/8), not a preference. Linking the system `libsqlite3` instead trades the compiler dependency for a runtime one.
- Cache sharing across concurrent herdr sessions is settled: one database, no per-pane duplication. The remaining piece of that fog patch is the per-pane memory/CPU budget.
- Threads over 100 comments paginate, so cached comments are stored per page with the fetched cursor; a re-fetch of a stale issue starts from the first page.
- Schema changes need a `user_version` check and a migration path, since old panes and new panes may share one file during an upgrade.
- Nothing here needs a background task, a timer, or a watcher. The pane is idle unless the user asks it for something.

## Alternatives rejected

- **One JSON file per repo** — simplest to inspect and to `rm`, no C dependency, but every write rewrites the whole file and concurrent panes on one repo race on it.
- **One file per issue** — many small files and syscalls, awkward to inspect.
- **One database per repo** — no cross-repo write contention, but N connections across N panes and no natural home for shared metadata like schema version.
- **TTL invalidation** — re-fetches unchanged issues and still serves silently-stale ones inside the window.
- **Explicit-only invalidation** — a thread that grew three comments looks identical until asked.
- **Size-capped LRU eviction** — a predictable ceiling, but access-time bookkeeping on every read for a corpus that is mostly a few MB of text.
