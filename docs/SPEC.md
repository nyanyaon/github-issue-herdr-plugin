# Build spec: read-only GitHub issue pane for herdr

This is the implementable spec for the plugin. It folds every decision on [the map](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/2) into one document. Where a decision has a rationale, it lives in the linked ADR; this file states *what to build*, not why.

Verified against **herdr 0.7.5, socket protocol 17**. Background facts: [`research/herdr-plugin-contract.md`](./research/herdr-plugin-contract.md), [`research/active-workspace-repo.md`](./research/active-workspace-repo.md), [`research/github-access-from-rust.md`](./research/github-access-from-rust.md).

## 1. What this is

A herdr plugin pane that shows **read-only** GitHub issues for the repo of the workspace it was opened in: issue list → issue body → comment thread. It exists because reading issues in a browser is too expensive on a laptop already running several agents, so the pane must be cheap: no browser, no idle work, no background polling.

Published to other herdr users via the marketplace.

**Out of scope, deliberately**: writes (comment, label, close), image rendering, a cross-repo inbox, pull requests, background polling or webhooks, GitHub Enterprise, Windows.

## 2. Repo layout

```
herdr-plugin.toml               manifest, repo root
Cargo.toml
src/
  main.rs                       startup, event loop, SIGWINCH/SIGHUP
  herdr.rs                      socket client (worktree.list)
  identity.rs                   repo_root → owner/repo
  github.rs                     GraphQL client + queries
  cache.rs                      SQLite open/migrate/read/write/prune
  config.rs                     config.toml
  ui/
    list.rs  detail.rs  markdown.rs  status.rs
scripts/fetch-or-build.sh
.github/workflows/release.yml
docs/                           ADRs and research (this directory)
README.md
```

The GitHub topic **`herdr-plugin`** must be set on the repo — that *is* the marketplace listing.

## 3. Manifest

```toml
id = "nyanyaon.herdr-issues"          # see §13 — settle before first release
name = "GitHub Issues"
version = "0.1.0"
min_herdr_version = "0.7.0"
description = "Read GitHub issues for the active workspace's repo, in a pane"
platforms = ["linux", "macos"]

[[build]]
platforms = ["linux", "macos"]
command = ["/bin/sh", "scripts/fetch-or-build.sh"]

[[panes]]
id = "issues"
title = "Issues"
placement = "split"
command = ["./bin/herdr-issues"]
```

`[[build]]` runs on `herdr plugin install` only, never on `plugin link`. The pane command's working directory is the plugin root, so the relative path resolves. No `[[startup]]`, no `[[events]]` — the plugin does nothing when its pane isn't open.

Optional later, additive: `[[link_handlers]]` matching `^https://github\.com/[^/]+/[^/]+/issues/[0-9]+$` so a modified-click on an issue URL in an agent pane opens it here.

## 4. Dependencies

| crate | why |
|---|---|
| `ureq` 3.3 (`json`, gzip default, rustls default) | blocking HTTP; one request in flight at a time. gzip is worth 14× on the list query |
| `serde`, `serde_json` | request/response bodies |
| `rusqlite` (`bundled`) | cache; `bundled` is why prebuilt binaries matter (§11) |
| `ratatui` + `crossterm` | full-screen TUI on a real PTY |
| `toml` | config file |

No tokio, no OpenSSL, no `octocrab`, no `graphql_client` codegen. Queries are hand-written strings.

## 5. Startup sequence

1. Read `HERDR_PLUGIN_CONTEXT_JSON` → `workspace_id`, `workspace_cwd`. Read `HERDR_SOCKET_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`.
2. Load `$HERDR_PLUGIN_CONFIG_DIR/config.toml` if present (§10).
3. Resolve the repo (§6). On failure, render the matching status state and stop fetching.
4. Open the cache (§8); run the startup prune.
5. Resolve the token (§7). On failure, render the no-token state; cached rows still display.
6. Render immediately from cache if present, then run the list query and re-render.

Signals: `SIGWINCH` → recompute layout and re-wrap. `SIGHUP` → flush, leave the alternate screen, exit 0. Exiting removes the pane; no teardown call is needed.

The token and the repo identity are resolved **once**, at startup.

## 6. Repo identity — [ADR-0004](./adr/0004-repo-identity.md)

1. Ask herdr: `worktree.list` with `{"workspace_id": "<id>"}` over `HERDR_SOCKET_PATH` (newline-delimited JSON, ~2 ms) → `result.source.repo_root`. Error `not_git_worktree` → the "no repo here" state. Fallback if the socket is unavailable: `git rev-parse --show-toplevel` plus `--git-common-dir` (linked worktrees do *not* collapse on their own — herdr does that for us).
2. Config override: `[repo."<repo_root>"] slug = "owner/repo"`, exact string match, wins over everything.
3. Otherwise `git -C <repo_root> remote get-url origin`; if there is no `origin` and exactly one remote exists, use that one; otherwise the ambiguous-remote state naming the candidates.
4. Parse `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)`, `ssh://git@github.com/o/r(.git)`. Any other host → the unsupported-host state naming the host.

The header displays the **`nameWithOwner` returned by the API**, not the parsed slug — GraphQL follows renames transparently.

## 7. Token — [ADR-0005](./adr/0005-token-discovery-and-failure-policy.md)

Order: `$GITHUB_TOKEN` → `$GH_TOKEN` → `gh auth token` (spawn, ~40 ms) → `token_file` from config (first line, trimmed) → no-token state.

The plugin never writes a credential. README asks for a fine-grained PAT with **Issues: Read-only** and **Metadata: Read-only**; classic `public_repo`/`repo` documented as the fallback.

## 8. GitHub client — [`github-access-from-rust.md`](./research/github-access-from-rust.md)

`POST https://api.github.com/graphql`, `Authorization: Bearer <token>`, gzip on. Two queries.

**List** (~4 KB wire, 16 KB parsed for 50 issues, 1 rate-limit point):

```graphql
query($owner:String!,$name:String!,$states:[IssueState!],$first:Int!,$after:String){
  repository(owner:$owner,name:$name){
    nameWithOwner
    issues(first:$first,after:$after,states:$states,orderBy:{field:UPDATED_AT,direction:DESC}){
      totalCount
      pageInfo{hasNextPage endCursor}
      nodes{number title state updatedAt comments{totalCount}
            author{login} labels(first:10){nodes{name color}}}
    }
  }
}
```

**Detail** (~22 KB wire, 66 KB parsed for 107 comments):

```graphql
query($owner:String!,$name:String!,$number:Int!,$first:Int!,$after:String){
  repository(owner:$owner,name:$name){
    issue(number:$number){
      number title body state createdAt updatedAt author{login}
      labels(first:20){nodes{name color}}
      comments(first:$first,after:$after){
        totalCount pageInfo{hasNextPage endCursor}
        nodes{author{login} createdAt body}
      }
    }
  }
}
```

Comments beyond the first page load on demand via `[m]ore`, passing `endCursor` as `after`.

Error handling: HTTP 401 → token-rejected state. HTTP 200 with `errors[0].type == "NOT_FOUND"` → not-found state (a private repo the token can't see is indistinguishable — one message covers both). HTTP 403 with rate-limit headers → rate-limited state with the reset time. Transport error → offline state. **Nothing retries automatically**; `r` is the retry.

## 9. Cache — [ADR-0001](./adr/0001-issue-cache-sqlite.md)

One database at `$HERDR_PLUGIN_STATE_DIR/cache.sqlite3`, shared by every pane. `PRAGMA journal_mode=WAL`, `PRAGMA busy_timeout=3000`, `PRAGMA user_version` for migrations.

```sql
CREATE TABLE repo (
  slug TEXT PRIMARY KEY,            -- nameWithOwner as returned by the API
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
```

**Invalidation is `updatedAt`-driven, never timed.** A detail is stale when `issue_list.updated_at != issue_detail.updated_at`; it re-fetches when the issue is *opened*, not eagerly, and a re-fetch drops that issue's cached comment pages and restarts at page 1.

**Prune at startup**: delete `issue_detail`/`issue_comments` rows with `touched_at` older than `prune_details_after_days` (30), and every row of a repo whose `opened_at` is older than `prune_repos_after_days` (90). `VACUUM` only when the file exceeds ~64 MB.

## 10. Configuration — [ADR-0003](./adr/0003-packaging-and-distribution.md)

`$HERDR_PLUGIN_CONFIG_DIR/config.toml`, absent by default; a fresh install needs no configuration.

```toml
list_page_size = 50               # default 50
detail_comment_page_size = 100    # default 100, GraphQL max
prune_details_after_days = 30
prune_repos_after_days = 90
token_file = "~/.secrets/gh-issues"   # optional, see §7

[repo."/home/me/work/fork"]
slug = "upstream-org/project"
```

Unknown keys are ignored with a warning line; a malformed file falls back to defaults rather than refusing to start.

## 11. UI — [ADR-0002](./adr/0002-pane-ui-shape.md)

Single column, drill-in. Keys are plain — herdr binds everything behind `ctrl+b`, so only `ctrl+b` and `ctrl+v` must be avoided.

**List view**

```
 github-issue-herdr-plugin · 6 open · fetched 3m ago            [o]pen ▾
────────────────────────────────────────────────────────────────────────
  #7   Pane UI shape                              prototype    ·   2m
▸ #8 ● Packaging and distribution for a publi…    grilling   1 ·  18m
────────────────────────────────────────────────────────────────────────
 j/k move   enter open   / filter   o state   r refresh   q close
```

`●` marks a row whose `updated_at` differs from the cached detail. Row: number, marker, truncated title, first label, comment count when non-zero, relative age. Keys: `j`/`k` (and arrows), `enter`, `/` filter, `o` cycle open→closed→all (re-fetch), `r` refresh list, `q` quit, `g`/`G` top/bottom.

`/` fuzzy-filters the **cached** list with no network, matching number, title, label and author; the header shows `3 of 6 shown`; `esc` clears.

**Detail view**: header line with `‹ #N title`, then `state · label · author · age · N comments` and the data age. Body, then comments separated by `── author · when ──`. Beyond the first page, a trailing `───── 7 more comments · [m]ore ─────`. Keys: `esc` back, `j`/`k` scroll, `n`/`p` next/previous issue in the filtered list, `m` load more, `r` re-fetch this issue, `q` quit.

**Markdown**: headings bold; lists bulleted with hang indent; fenced code dimmed behind a `│` gutter; inline code reversed; links as text with the target dimmed. Tables and images degrade to raw source. Everything wraps to pane width and re-wraps on `SIGWINCH`.

**States** — one status line, never a modal, cached rows always left on screen:

| state | line |
|---|---|
| offline | `offline · showing cache from 4h ago` |
| token missing | `no GitHub token found · set GITHUB_TOKEN, run \`gh auth login\`, or set token_file` |
| token rejected (401) | `token rejected · check GITHUB_TOKEN or run \`gh auth login\`` |
| not found | `nyanyaon/foo not found — or your token can't see it` |
| rate limited | `rate limited · resets in 12m · [r] retry` |
| no repo | `no git repo in this workspace (<workspace_cwd>)` |
| ambiguous remote | `several remotes and no origin: <a>, <b> · set slug in config.toml` |
| unsupported host | `gitlab.com is not supported — github.com only` |
| empty | `no open issues · [o] to include closed` |

Cache-first always: existing data renders immediately with its age while a refresh runs; no spinner over content that already exists. A cold start with no cache shows a single status line.

## 12. Performance budget

The pane must be cheap enough that several are open at once on a loaded laptop:

- **Idle CPU: zero.** No timers, no polling, no watcher, no subscription. The process blocks on terminal input between renders. Relative ages are computed at render time only.
- **Memory**: target < 30 MB RSS with a 50-issue list and one open thread. The largest live allocation is one parsed detail response (~66 KB for 107 comments).
- **Per refresh**: ~4 KB wire / 16 KB parsed for a list, ~22 KB / 66 KB for a long thread, 1 rate-limit point each, ~1.0–1.4 s round trip.
- **Startup to first paint with a warm cache**: no network on the critical path — read cache, draw, then fetch.
- **Never subscribe to `pane.updated`** (measured 69 messages in 6 seconds on a near-idle session).

## 13. Packaging — [ADR-0003](./adr/0003-packaging-and-distribution.md)

`scripts/fetch-or-build.sh`:

1. Read `version` from `herdr-plugin.toml`.
2. Detect target (`uname -s`/`-m` → `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`, `aarch64-apple-darwin`).
3. `curl -fsSL <releases>/v$VERSION/herdr-issues-$TARGET.tar.gz`, verify against `checksums.txt`, unpack to `./bin/herdr-issues`, `chmod +x`.
4. On **any** failure — no release, no network, checksum mismatch, unknown target — fall back to `cargo build --release` and copy the binary to `./bin/herdr-issues`.

`.github/workflows/release.yml`, on `push` of tag `v*`: build all four targets, write `checksums.txt`, create the release. **Fail the workflow if the tag and the manifest `version` disagree** — the fetch script keys off the manifest version.

Dev loop: `cargo build --release` + `herdr plugin link .` (link never runs `[[build]]`).

**Settle before the first release**: the plugin `id`. It names `HERDR_PLUGIN_CONFIG_DIR` and `HERDR_PLUGIN_STATE_DIR`, so changing it later orphans user config and cache. `nyanyaon.herdr-issues` is the working proposal.

## 14. README must state

Install (`herdr plugin install nyanyaon/github-issue-herdr-plugin`), opening the pane (`herdr plugin pane open --plugin <id> --entrypoint issues`, plus the `[[keys.command]]` `plugin_action` binding), the token options in the order of §7, the fine-grained token permissions, that it is read-only, and that linux/macOS are supported with Windows falling back to a source build.

## 15. Definition of done for v1

A user with `gh` installed runs `herdr plugin install`, binds a key, opens the pane in a workspace whose repo is on github.com, and reads issue titles, bodies and comment threads without opening a browser — with the pane costing nothing while idle.
