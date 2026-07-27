# Context: read-only GitHub issue pane for herdr

One plugin, one bounded context. The full build spec is [`docs/SPEC.md`](./docs/SPEC.md); decisions live in [`docs/adr/`](./docs/adr/) and the facts behind them in [`docs/research/`](./docs/research/).

## Ubiquitous language

Two vocabularies meet here — herdr's and GitHub's — and they collide on the word "issue". Use these terms exactly; don't drift to the synonyms listed as avoided.

### From herdr

| Term | Meaning |
|---|---|
| **Workspace** | A herdr workspace, id `w3`. The pane lives in exactly one, fixed for its process lifetime. |
| **Pane** | A herdr pane, id `w3:p1` — a real PTY running one command. Ours runs the viewer binary. |
| **Plugin root** | `HERDR_PLUGIN_ROOT`, the plugin directory and the pane command's working directory. Never holds state or credentials. |
| **State dir** / **config dir** | `HERDR_PLUGIN_STATE_DIR` (the cache) and `HERDR_PLUGIN_CONFIG_DIR` (`config.toml`). Both keyed by plugin id. |
| **Repo root** | The `source.repo_root` herdr returns from `worktree.list`. Linked worktrees collapse to it. _Avoid: "project dir", "checkout"._ |

### Ours

| Term | Meaning |
|---|---|
| **Viewer** | The Rust binary this repo builds — `bin/herdr-issues`. _Avoid: "the app", "the client"._ |
| **Slug** | A GitHub `owner/repo`. Always the `nameWithOwner` the API returned, never the string parsed from a remote. |
| **Repo identity** | The resolution of a repo root to a slug: config override → `origin` → sole remote. See [ADR-0004](./docs/adr/0004-repo-identity.md). |
| **List view** / **detail view** | The two screens. There is no third. _Avoid: "index", "issue page"._ |
| **Issue list** | The cheap GraphQL query and its cached rows — number, title, state, `updatedAt`, labels, comment count. |
| **Issue detail** | Body plus comment pages for one issue. Cached separately from the list, and separately invalidated. |
| **Stale** | A detail whose cached `updatedAt` differs from the list's. Shown as `●`. Staleness is never a clock. _Avoid: "expired", "TTL"._ |
| **Refresh** | A user-initiated fetch (`r`). Nothing else fetches. _Avoid: "sync", "poll" — the plugin does neither._ |
| **Prune** | The startup deletion of details untouched for 30 days and repos unopened for 90. Distinct from **refresh**. |
| **Status line** | The single line carrying every failure and empty state. There are no modals. |

### Deliberately absent

**Triage, label, assign, close, comment** are GitHub issue *actions* — this viewer is read-only, so the words describe things it displays, never things it does. The repo's own triage vocabulary (`needs-triage`, `ready-for-agent`, …) belongs to [`docs/agents/triage-labels.md`](./docs/agents/triage-labels.md) and governs this repo's issues, not the viewer's behaviour.

**Ticket** and **map** are wayfinder's words for this repo's planning issues — never the viewer's word for a GitHub issue it renders.

## Invariants

- The pane does **no work while idle**: no timer, no poll, no watcher, no held subscription.
- Cache-first: cached data renders before any network call, and a failed fetch never clears it.
- Read-only: the viewer issues no mutating GitHub request, ever.
- One repo per pane, bound at startup and never re-resolved.
