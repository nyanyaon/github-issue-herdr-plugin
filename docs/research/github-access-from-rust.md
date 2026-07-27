# How should the plugin talk to GitHub from Rust?

Research asset for [How should the plugin talk to GitHub from Rust?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/5).

Every byte count and timing below was measured live on 2026-07-27 against `api.github.com` from this laptop, using `cli/cli` as the stress corpus (issue #326 has **107 comments**; the open-issue list is large). Wire sizes were taken with `curl --raw -H 'Accept-Encoding: gzip'` and verified as genuinely gzipped by the `1f8b` magic bytes; raw sizes are the decompressed bodies.

## Recommendation

**GraphQL, over `ureq` + `serde`, with the token from `gh auth token`.**

| | wire (gzip) | raw JSON to parse | requests |
|---|---|---|---|
| Issue list, 50 items — REST | 59,119 B | 292,444 B | 1 |
| Issue list, 50 items — **GraphQL** | **4,097 B** | **16,339 B** | 1 |
| Issue + 107 comments — REST | 39,137 B (+ page 2) | 247,437 B | 1 + ⌈n/100⌉ |
| Issue + 107 comments — **GraphQL** | **21,788 B** | **65,659 B** | 1 |

GraphQL wins on every axis that matters here: **14× less wire** and **18× less JSON to deserialize** for the list, **1.8× less wire** and **3.8× less parsing** for a long thread, and it collapses issue+comments into a single round trip. Since the whole premise of this plugin is "the laptop is already loaded", the parse column is the one that decides it — REST makes the pane deserialize a quarter-megabyte of JSON that is mostly URL templates and reaction counters we never render.

Rate limit is a non-issue: **GraphQL cost is 1 point per query** (measured via `rateLimit{cost}`) out of 5,000/hour. With manual refresh, that is thousands of refreshes per hour.

## Route comparison

### `gh` CLI shell-out — rejected

Measured `gh issue list --repo cli/cli --limit 50 --json …`: **best 2,928 ms / avg 3,475 ms**, versus ~1.4 s for either direct route. `gh issue view --comments --json …` took **3,505 ms**. The overhead is not process spawn — `gh auth token` alone is a steady **40 ms** — it is `gh` making its own multiple API calls and materialising fields we didn't ask for.

It also imports a hard dependency on the user's `gh` install into a *published* plugin, and gives no control over field selection. Rejected as a data path; kept as an **auth** path (below), where its 40 ms cost buys real value.

### REST — rejected as primary

Fine API, and it has one property GraphQL lacks (conditional requests, below), but the payloads are dominated by fields we never render, and a long thread needs `1 + ⌈comments/100⌉` requests.

### GraphQL — chosen

One query returns list-or-detail with exactly the fields the pane draws. Verified working query shapes:

```graphql
# list
query {
  repository(owner: "cli", name: "cli") {
    issues(first: 50, states: OPEN, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes { number title state updatedAt comments { totalCount }
              author { login } labels(first: 10) { nodes { name color } } }
    }
  }
  rateLimit { cost remaining }
}

# detail
query {
  repository(owner: "cli", name: "cli") {
    issue(number: 326) {
      number title body state createdAt author { login }
      labels(first: 20) { nodes { name color } }
      comments(first: 100) {
        totalCount pageInfo { hasNextPage endCursor }
        nodes { author { login } createdAt body }
      }
    }
  }
}
```

**[verified]** `comments.pageInfo.hasNextPage` is `true` for the 107-comment issue, so cursor pagination is required for threads over 100 — one extra query per additional 100 comments.

## The one thing REST has: free revalidation

**[verified]** REST conditional requests are **free**. Reading `x-ratelimit-used` across a sequence:

```
GET (200)          used=12
GET If-None-Match  used=12   → HTTP 304
GET If-None-Match  used=12   → HTTP 304
GET (200)          used=13
```

Two 304s consumed **zero** quota; a fresh 200 consumed one. GraphQL has no ETag equivalent — every query costs its point and returns a full body.

This does **not** overturn the choice, because GraphQL's quota is effectively unlimited for manual refresh, and because GraphQL offers a better staleness signal anyway: the list query already returns `updatedAt` per issue for 4 KB, so the cache can re-fetch only the issues whose `updatedAt` moved. That is finer-grained than an all-or-nothing 304. Hand the trade-off to [Cache and refresh policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/6).

## Auth

**[verified] `gh auth token` is the right acquisition path where `gh` exists.** It costs 40 ms, and it already resolves the whole precedence chain — `GH_TOKEN` in the environment overrides everything (verified: `GH_TOKEN=dummy gh auth token` → `dummy`), otherwise it reads `~/.config/gh/hosts.yml` or the system keyring, whichever that install uses. Reimplementing that in Rust means reimplementing its bugs; `hosts.yml` holds `oauth_token` in plaintext at mode `600`, but only for installs that don't use secure storage, so reading it directly is a fallback, never the primary.

**[verified] Scopes and failure modes:**

- GraphQL **requires** authentication: unauthenticated `POST /graphql` returns **403**. There is no anonymous mode to degrade to.
- `x-accepted-oauth-scopes: repo` on the GraphQL endpoint; the session token carries `repo` and reads private repos fine.
- Unauthenticated REST is capped at **60 requests/hour** (`x-ratelimit-limit: 60`), versus 5,000 authenticated.

For a *published* plugin, the user-without-`gh` case is a real decision — env var only, a token file under `HERDR_PLUGIN_CONFIG_DIR`, or an OAuth device flow — as is what the pane does on 401/403/rate-limit. Split out as [Token discovery and API failure policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/11).

## Crates

**`ureq` 3.3.0** (July 2026) + `serde`/`serde_json`, hand-written GraphQL query strings.

- **Blocking**, by design — "keeps the API simple and keeps dependencies to a minimum". A manual-refresh TUI has exactly one in-flight request at a time; a tokio runtime in every pane is pure overhead against the perf budget.
- **gzip on by default** — non-negotiable given the 14× wire difference above.
- **rustls by default** (ring provider), no OpenSSL — which also makes the static/cross-compiled binaries that [Packaging and distribution](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/8) will want far easier.
- serde JSON via `send_json()` / `read_json()`; proxy support from `HTTPS_PROXY`/`NO_PROXY`; global timeout via `Agent::config_builder().timeout_global()`.

Rejected: **`octocrab`** (async/tokio, REST-shaped, heavy tree for a program that issues one request at a time) and **`reqwest` blocking** (still drags in hyper + tokio). **`graphql_client`** codegen was considered and rejected — two hand-written queries do not justify a schema download and a build-time codegen step in a plugin that must build on the installing user's machine.

## What this pins down for the map

1. Transport is **GraphQL** with two hand-written queries; REST is not used for reads.
2. Budget per refresh: ~4 KB wire / 16 KB parse for a 50-issue list, ~22 KB wire / 66 KB parse for a 107-comment thread, 1 rate-limit point each, ~1.0–1.4 s round trip on this connection.
3. Threads over 100 comments need cursor pagination — a UI decision (load-more vs fetch-all) for [Pane UI shape](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/7).
4. `updatedAt` from the cheap list query is the cache's staleness signal; REST's free 304s are the fallback if that proves insufficient.
5. Stack: `ureq` 3.3 (blocking, rustls, gzip) + `serde_json`. No tokio, no OpenSSL.
6. Auth is `gh auth token` where available; everything else is [Token discovery and API failure policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/11).
