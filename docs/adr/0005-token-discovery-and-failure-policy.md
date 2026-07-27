# ADR-0005: Token discovery order, and failing soft on every API error

- **Status**: accepted
- **Date**: 2026-07-27
- **Ticket**: [Token discovery and API failure policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/11)

## Context

GitHub GraphQL **requires** authentication — an unauthenticated `POST /graphql` returns 403, with no anonymous mode to degrade to ([`github-access-from-rust.md`](../research/github-access-from-rust.md)). The plugin ships to strangers, so "run `gh auth login` first" cannot be the only answer.

Error shapes were probed live:

- **Bad token** → HTTP **401**, body `{"message": "Bad credentials", …}`.
- **Missing repo** → HTTP **200** with `data.repository: null` and `errors[0].type: "NOT_FOUND"`. A **private repo the token cannot see returns the same NOT_FOUND** — GitHub does not leak existence, so the two cases are indistinguishable to us and must share one message.

## Decision

### Discovery order

1. `$GITHUB_TOKEN`, then `$GH_TOKEN`.
2. `gh auth token` — 40 ms, and it already resolves `GH_TOKEN`, `hosts.yml`, or the system keyring, whichever that install uses.
3. `token_file = "<path>"` from `$HERDR_PLUGIN_CONFIG_DIR/config.toml`; read the first line, trimmed.
4. Otherwise, a status line:

```
no GitHub token found
set GITHUB_TOKEN, run `gh auth login`, or set token_file in
~/.config/herdr/plugins/config/<id>/config.toml
```

No embedded OAuth app and no credential of our own. An OAuth **device flow** would be friendlier for a stranger installing from the marketplace, but it means shipping a client id, owning token storage and refresh, and writing a credential we then become responsible for — too much surface for a read-only viewer whose users overwhelmingly already have `gh`.

The token is resolved **once at startup**; the resolved source is not displayed unless it fails.

### Recommended token: fine-grained, read-only

The README asks for a **fine-grained personal access token** with repository permissions **Issues: Read-only** and **Metadata: Read-only** (Metadata is required alongside Issues), scoped to whichever repos the user wants. A read-only viewer requesting `repo` — read *and write* on everything — is indefensible for a published plugin.

Documented fallback for orgs that have not enabled fine-grained tokens, or for users who just ran `gh auth login`: a classic PAT with `public_repo` for public repos, `repo` for private ones.

### Failure policy: fail soft, one line, manual retry

Cached rows stay on screen in every failure case. The status line names the specific failure. **Nothing retries by itself** — `r` is the retry, consistent with a pane that does no work you did not ask for.

| case | line |
|---|---|
| 401 | `token rejected · check GITHUB_TOKEN or run \`gh auth login\`` |
| NOT_FOUND | `nyanyaon/foo not found — or your token can't see it` |
| rate limited | `rate limited · resets in 12m · [r] retry` |
| network | `offline · showing cache from 4h ago` |

The NOT_FOUND wording deliberately covers both readings, because the API gives us no way to tell them apart.

### Rate-limit quota is not surfaced

GraphQL costs **1 point per query** out of 5,000/hour. Remaining quota is displayed **only** when a request is actually rate-limited, with the reset time; a permanent counter would be noise about a limit manual refresh cannot realistically reach.

## Consequences

- Startup cost in the worst case is one 40 ms `gh auth token` spawn — only when neither environment variable is set.
- The plugin never writes a credential. `token_file` points at a file the user already owns and manages.
- Because the token resolves once at startup, a token that expires mid-session surfaces as 401 on the next refresh and is fixed by reopening the pane. Acceptable for a v1; a re-resolve on 401 is additive.
- A private repo without permission and a typo'd repo name are the same on screen. The message says so rather than guessing.
- Nothing in this ADR reintroduces background work: no polling for quota, no retry timers, no refresh loop.

## Alternatives rejected

- **OAuth device flow** — best onboarding for a stranger, but embeds a client id and makes us the custodian of a credential.
- **`gh auth token` only** — one source, zero ambiguity, but hard-requires `gh` for a plugin whose data path deliberately avoids it, and shuts out CI and container users.
- **Auto-retry with backoff** — recovers unattended when the network returns, but reintroduces background work into a pane whose selling point is that it is idle unless asked.
- **Blocking error screen** — impossible to miss, but throws away good cached data over a transient hiccup.
- **Classic PAT with `repo` as the recommendation** — one checkbox and universally available, but grants write access across every repo for a viewer that never writes.
