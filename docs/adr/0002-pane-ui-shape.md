# ADR-0002: Single-column drill-in pane with light markdown styling

- **Status**: accepted
- **Date**: 2026-07-27
- **Ticket**: [Pane UI shape](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/7)

## Context

The pane is a full-screen TUI on a real PTY with `SIGWINCH` on resize ([`herdr-plugin-contract.md`](../research/herdr-plugin-contract.md)). It opens at whatever width herdr gives it — a `split` pane next to an agent is routinely half the terminal, and herdr's own sidebar takes ~26 columns before the split is even divided.

Layout options were prototyped against this repo's real issues at 72 columns, the width of a half-width pane on a 179-column terminal.

## Decision

### Keybindings are plain keys

**[verified]** Every herdr binding is prefix-mode (`ctrl+b` by default): `prefix+n`, `prefix+x`, `prefix+h/j/k/l`, and so on. The only non-prefix bindings are navigate-mode movement keys, which apply only while herdr's navigate mode is open, and `ctrl+v` for image paste under `herdr --remote`. So plain keys reach the pane process untouched.

The plugin therefore binds plain keys and **must avoid `ctrl+b` and `ctrl+v`**.

### Layout: single column, drill-in

The list fills the pane; `enter` replaces it with a full-pane issue view; `esc` returns. No internal split — herdr already provides real splits if the user wants the list beside something else, and a split inside a half-width pane leaves ~35 columns of prose.

```
 github-issue-herdr-plugin · 6 open · fetched 3m ago            [o]pen ▾
────────────────────────────────────────────────────────────────────────
  #7   Pane UI shape                              prototype    ·   2m
  #2   Map: read-only GitHub issue pane for h…    map          ·   3m
▸ #8 ● Packaging and distribution for a publi…    grilling   1 ·  18m
  #11  Token discovery and API failure policy     grilling     ·  22m
  #10  Repo identity: which remote names the …    grilling     ·  35m
  #9   Assemble the build spec                    grilling     ·   4h
────────────────────────────────────────────────────────────────────────
 j/k move   enter open   / filter   o state   r refresh   q close
```

Row anatomy: number, a `●` marker when the cached detail is behind the list's `updatedAt` (per [ADR-0001](./0001-issue-cache-sqlite.md)), the title truncated with `…`, the first label, the comment count when non-zero, and a relative age. The header always carries repo, open count and data age.

The detail view keeps the same chrome:

```
 ‹ #8 Packaging and distribution for a publishable Rust plugin
 open · grilling · nyanyaon · 18m ago · 1 comment          fetched 3m ago
────────────────────────────────────────────────────────────────────────
 Part of #2

 ## Question
 …
 ── nyanyaon · 18m ago ──────────────────────────────────────────────────
 Constraint arriving from Cache and refresh policy: the cache is
 SQLite (ADR-0001). rusqlite with bundled compiles SQLite from C, so…
────────────────────────────────────────────────────────────────────────
 esc back   j/k scroll   n/p next issue   r refresh   q close
```

### Markdown: light styling, wrapped to width

Headings bold, lists bulleted with hang indent, fenced code dimmed behind a `│` gutter, inline code reversed, links as text with the target dimmed after it. **Tables and images degrade to their raw source** — no alignment engine, no image protocol (images are out of scope for this map). Everything wraps to the pane width and re-wraps on `SIGWINCH`.

### Filters: state toggle plus local fuzzy filter

`o` cycles open → closed → all and re-fetches. `/` fuzzy-filters the **cached** list — no network — matching title, number, label and author at once, so labels need no separate picker. The header shows `3 of 6 shown` while a filter is active; `esc` clears it.

Rejected: a dedicated label/assignee picker (more UI, more header state) and a GitHub-search box (a round trip per keystroke, against search's own 30/minute limit).

### Long threads: first 100, load more on demand

A GraphQL query returns 100 comments; anything beyond that ends the view with a `───── 7 more comments · [m]ore ─────` line that fetches the next page when pressed. One round trip to open any issue, in original reading order.

### Loading, empty, error

Cache-first, always. If a repo has cached data the pane renders it immediately and the header reports its age while a refresh runs behind it — no spinner over content that already exists. A cold start with no cache shows a single status line, not a skeleton.

- **Offline / failed refresh** — cached rows stay, header reads `fetched 4h ago · offline` (ADR-0001).
- **No repo in this workspace** — the `not_git_worktree` case: one line naming the workspace and its directory.
- **Zero open issues** — `no open issues · [o] to include closed`.
- **No token, 401/403, rate-limited** — [Token discovery and API failure policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/11) decides the wording and the recovery path; this ADR only fixes where it appears: the same one-line status area, never a modal.

## Consequences

- One layout to build and test, not two. If a width threshold for a side-by-side variant is ever wanted, it is additive.
- The fuzzy filter needs the whole list in memory — fine, since it is 16 KB parsed for 50 issues.
- `q` closes the pane; because the pane's command exiting removes the pane (verified in the plugin contract), no extra teardown call is needed.
- The `●` staleness marker ties the list row to the cache state, so the list query and cache must agree on `updatedAt` per issue.
- Relative ages in the header and rows mean a redraw on a timer *would* be needed to keep them honest — instead, they are computed at render time only, and the pane stays idle otherwise.

## Alternatives rejected

- **List/detail split inside the pane** — good at full width, cramped below ~100 columns.
- **Adaptive split above a width threshold** — best of both, but two layouts to build and keep consistent for a v1.
- **Full-fidelity markdown with aligned tables and syntax highlighting** — prettier on long design docs, much bigger dependency, more to go wrong at narrow widths.
- **Raw markdown, wrapped only** — perfectly faithful, but `##`, `**` and fence markers are noise on screen.
- **Newest-100-first with older collapsed** — matches how long threads are usually read, but reverses the reading order everywhere else.
- **Fetching every comment page up front** — no load-more, but a 400-comment issue costs four round trips before anything renders.
