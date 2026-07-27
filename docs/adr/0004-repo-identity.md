# ADR-0004: Repo identity resolves origin-first, github.com only

- **Status**: accepted
- **Date**: 2026-07-27
- **Ticket**: [Repo identity: which remote names the repo?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/10)

## Context

Herdr resolves a workspace to a `repo_root` but knows nothing about remotes, and `repo_name` is a directory label rather than an identity — the live session had two workspaces both labelled `simata-start` ([`active-workspace-repo.md`](../research/active-workspace-repo.md)). The GitHub `owner/repo` has to come from the git remote.

Surveying the 26 repos in this user's home directory: **24 have exactly one remote, named `origin`**. The two exceptions are instructive:

- `gdal3.js` has two remotes — `origin` pointing at the **upstream** `bugra9/gdal3.js`, plus a `nyaon` remote for the personal fork. A "prefer `upstream`" rule would read the wrong repo here, or nothing at all.
- `landcat-extension` has **no remote at all**.

## Decision

### Resolution order

1. **Config override** — `[repo."<repo_root>"] slug = "owner/repo"` in `$HERDR_PLUGIN_CONFIG_DIR/config.toml`.
2. **`origin`**, if a remote by that name exists.
3. **The only remote**, if there is exactly one and it isn't named `origin`.
4. Otherwise an error line naming the candidate remotes, pointing at the config key.

`origin` wins over any `upstream` convention. On real data it is right 24 times out of 26, and in the one multi-remote case `origin` already points at the repo whose issues you would read. A rule that prefers `upstream` would invert exactly that case while silently ignoring a remote the user named deliberately.

### Keyed by repo root path

The override key is the **`repo_root`** herdr hands back from `worktree.list` — an exact string match. Linked worktrees already collapse to their source repo root, so one entry covers every worktree of a repo. Keying by remote URL was rejected: it survives moving a checkout, but fails for the no-remote case, which is precisely where an override is most needed.

### github.com only

The remote's host is parsed from all three URL forms — `https://github.com/o/r.git`, `git@github.com:o/r.git`, `ssh://git@github.com/o/r`. Anything else gets a plain unsupported line naming the host: `gitlab.com is not supported — github.com only`, or `remote has no github.com host` for a bare path.

GitHub Enterprise would need only a different GraphQL endpoint (`https://<host>/api/graphql`) and a per-host token (`gh auth token --hostname <host>`) — small, but untestable from here. An honest unsupported message beats an untested code path; the design is recorded so it stays additive.

### Renames need no handling

**[verified]** GraphQL follows renames transparently: `repository(owner: "denoland", name: "deno_std")` returns `nameWithOwner: "denoland/std"`. REST answers 301 for the same slug.

Consequently the pane displays the **`nameWithOwner` from the response**, not the slug parsed from the remote. A repo renamed on GitHub keeps working with a stale remote, and the header shows its current name.

## Consequences

- Identity resolution is: config lookup → `git remote get-url` (or the remote list when `origin` is absent) → host and path parse. One `git` invocation in the common case, on top of the single `worktree.list` call that yields `repo_root`.
- Every failure mode is a named, one-line state in the pane's status area ([ADR-0002](./0002-pane-ui-shape.md)): no remotes, several remotes with no `origin`, non-github.com host, no repo in this workspace (`not_git_worktree`).
- The `[repo."<path>"]` table now has a second use beyond overrides — it is the only way to give the no-remote case (`landcat-extension`) an identity.
- Requesting `nameWithOwner` in the list query costs nothing measurable and buys rename tolerance plus a correct header.
- Moving a checkout invalidates its override entry. Acceptable: the entry is one line, and the failure is visible rather than silent.

## Alternatives rejected

- **`upstream` if present, else `origin`** — the classic fork convention, but it inverts on repos like `gdal3.js` where `origin` is already upstream, and it ignores a deliberately named remote.
- **Ask once and remember** — never guesses wrong, but adds a modal picker to a read-only viewer for a case that hits 1 repo in 26.
- **Keyed by remote URL** — covers every clone of a repo at once, but has nothing to key on when there is no remote.
- **Supporting GitHub Enterprise now** — real value for work repos, zero ability to test it here.
