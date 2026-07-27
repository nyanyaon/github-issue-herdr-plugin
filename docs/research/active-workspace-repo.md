# How does a plugin learn the active workspace repo?

Research asset for [How does a plugin learn the active workspace repo?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/4).

**Read against herdr `0.7.5`, socket protocol `17`.** Every number and payload below was observed live against the running server; the manifest/env facts it builds on come from [`herdr-plugin-contract.md`](./herdr-plugin-contract.md).

## Recommendation

Bind the repo **once, at pane launch**, in three cheap steps:

1. **Env, free.** `HERDR_PLUGIN_CONTEXT_JSON` already carries `workspace_id`, `workspace_cwd`, `focused_pane_id`, `focused_pane_cwd`. No call needed to know *which workspace* the pane belongs to.
2. **One socket round trip, ~2 ms.** Send `worktree.list` with `{"workspace_id": "<id>"}` to `HERDR_SOCKET_PATH`; read `result.source.repo_root` / `repo_name` / `repo_key`.
3. **One `git` invocation.** `git -C <repo_root> remote get-url origin` → parse `owner/repo`. Herdr exposes no remote URL, so this step is unavoidable.

Then **stop watching**. Change detection is not needed in v1 — see [Why no change detection](#why-no-change-detection).

Total startup cost: one ~2 ms socket call plus one `git` process. Zero idle cost — nothing to poll, no subscription held open.

## Step 1 — what the environment already gives you

**[verified]** A pane entrypoint receives `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID`, `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH` and:

```json
{"workspace_id":"w1","workspace_label":"simata-start","workspace_cwd":"/home/nyaon/simata-start",
 "tab_id":"w1:t3","tab_label":"geometry edit guard","focused_pane_id":"w1:p3",
 "focused_pane_cwd":"/home/nyaon/simata-start","focused_pane_agent":"claude",
 "focused_pane_status":"working","invocation_source":"api","correlation_id":"plugin-pane"}
```

`workspace_cwd` alone would nearly do — but it is the workspace's directory, not necessarily a repo root, and it does not collapse linked worktrees. Step 2 fixes both.

## Step 2 — `worktree.list` resolves the repo

**[verified]** Newline-delimited JSON over the unix socket at `HERDR_SOCKET_PATH`. Request:

```json
{"id":"x","method":"worktree.list","params":{"workspace_id":"w3"}}
```

Reply (trimmed):

```json
{"result":{"type":"worktree_list",
  "source":{"repo_key":"/home/nyaon/simata-start/.git","repo_name":"simata-start",
            "repo_root":"/home/nyaon/simata-start",
            "source_checkout_path":"/home/nyaon/simata-start","source_workspace_id":"w1"},
  "worktrees":[{"branch":"feat/530-warn-on-close-when-unsaved","is_linked_worktree":false,
                "open_workspace_id":"w1","path":"/home/nyaon/simata-start"},
               {"branch":"worktree-issue-489-contrast-floors","is_linked_worktree":true,
                "path":"/home/nyaon/simata-start/.claude/worktrees/issue-489-contrast-floors"}]}}
```

`WorktreeListParams` accepts `workspace_id` **or** `cwd` (both optional; with neither, it resolves against the active workspace — verified: run from an unrelated directory it still answered for the focused workspace, not the shell's cwd).

Measured, three consecutive calls: **2.1 ms, 4.6 ms, 1.6 ms**. `session.snapshot` for comparison: 5.8 ms for a 10 KB payload — usable, but `worktree.list` is the narrower question.

Two behaviours that matter:

- **[verified] Linked worktrees collapse to their source repo.** Asking with `cwd = /home/nyaon/simata-start/.claude/worktrees/issue-489-contrast-floors` returns `repo_root = /home/nyaon/simata-start`. So every worktree of a repo shows the *same* issue list — the behaviour we want, for free.
- **[verified] Non-repo directories fail cleanly**: `{"error":{"code":"not_git_worktree","message":"Herdr worktree actions require a path inside a Git work tree"}}`. That is the "this workspace has no repo" state, named and detectable.

**Fallback** when the socket is unavailable (plugin binary run outside herdr, or a `--cwd` override that points elsewhere): `git rev-parse --show-toplevel` from `workspace_cwd`, or from the process cwd. Note that `git rev-parse --show-toplevel` inside a linked worktree returns the *worktree* path, not the source repo — the collapsing behaviour is herdr's, not git's, so the fallback also needs `git rev-parse --git-common-dir` to reach the source.

## Step 3 — repo root to `owner/repo`

Herdr knows nothing about remotes. `repo_name` is a directory label (`simata-start`), not a GitHub slug, and it is **not unique**: the live session had two workspaces, w1 and w2, both labelled `simata-start`.

**[verified]** `git -C <repo_root> remote get-url origin` → `https://github.com/nyanyaon/simata-start.git`, and it works from inside a linked worktree too (worktrees share the repo config).

Which remote counts, and what happens with none / several / a non-GitHub one, is a decision, not a fact — split out as [Repo identity: which remote names the repo?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/10).

## Why no change detection

A plugin pane **lives inside one workspace** — `HERDR_WORKSPACE_ID` is fixed for the process lifetime. The user "switching workspaces" does not move the pane; it hides it. Opening a linked worktree produces a *different* workspace (`open_workspace_id` on the worktree entry), which would get its own pane.

So the repo a pane displays cannot change under it — except by the user `cd`-ing inside the workspace, which does not change the workspace's own repo binding anyway. **Resolve once at startup, keep it.**

If a later version wants to repaint when the pane becomes visible again, the mechanism exists and is verified:

**[verified]** `events.subscribe` over the same socket streams live events:

```json
{"id":"sub","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.focused"},{"type":"worktree.opened"}]}}
```

→ `{"id":"sub","result":{"type":"subscription_started"}}`, then a stream of
`{"data":{"type":"workspace_focused","workspace_id":"w5"},"event":"workspace_focused"}`.

Subscription names are dotted (`workspace.focused`); the events that come back are snake_case (`workspace_focused`). `events.wait` with an `EventMatch` and `timeout_ms` is the one-shot variant.

**Do not subscribe to `pane.updated`.** Measured **69 messages in 6 seconds** on an idle-ish session with `{workspace.focused, worktree.opened, pane.updated}` subscribed — almost all of it pane chatter from running agents. Deserialising that in every open pane is exactly the idle cost this plugin exists to avoid.

## What this pins down for the map

1. Repo binding is: env → one `worktree.list` call (~2 ms) → one `git remote` call. Nothing else.
2. Linked worktrees resolve to the source repo automatically, so the pane shows one issue list per repo, not per worktree.
3. `not_git_worktree` is the canonical "no repo here" signal.
4. No subscription, no polling, no watcher in v1 — the pane's workspace, hence its repo, is fixed at launch.
5. `repo_name` is a label, not an identity; `owner/repo` comes from the git remote.
