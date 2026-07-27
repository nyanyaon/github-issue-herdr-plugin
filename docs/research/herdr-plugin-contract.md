# What is a herdr plugin?

Research asset for [What is a herdr plugin?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/3).

**Read against herdr `0.7.5`, socket protocol `17`, schema_version `1`** (`herdr --version`, `herdr status`). Sources: [herdr.dev/docs/plugins/](https://herdr.dev/docs/plugins/), the bundled API schema (`herdr api schema --json`), `herdr plugin --help`, and a probe plugin linked and opened against the live server — findings marked **[verified]** were observed directly on this machine.

## Summary

A plugin is **a directory containing `herdr-plugin.toml` plus ordinary executables**. Herdr owns the host surface (panes, keys, events, links); the plugin owns its implementation language. There is no SDK and no sandbox: "the entire Herdr CLI is the plugin API", and plugin commands run as the user, with the user's environment.

For this map that means a **Rust binary launched as a pane command, drawing into a real PTY** — a normal TUI program, no herdr-specific rendering protocol to implement.

## Manifest — `herdr-plugin.toml`

Top level, required: `id` (ASCII letters/digits/`.`/`:`/`_`/`-`), `name`, `version` (semver), `min_herdr_version`.
Top level, optional: `description`, `platforms` (`["linux","macos","windows"]`).

Entrypoint tables, all optional and repeatable:

| Table | Purpose |
|---|---|
| `[[build]]` | `command`, optional `platforms`. Runs on **GitHub install only**, after user confirmation; **not** on `plugin link`. Failure aborts install. Gets no runtime/socket env. Herdr does not install toolchains. |
| `[[startup]]` | `command`. Runs once per enabled plugin after session restore and once the API socket is ready; again on live handoff; **not** on client attach, config reload, or link/enable. Async; failure does not stop the server. Receives `HERDR_PLUGIN_EVENT=startup`. Shown in the install preview. |
| `[[actions]]` | `id`, `title`, `contexts` (e.g. `["workspace"]`), `command`. One-shot. Invocable via `herdr plugin action invoke <plugin>.<action>`, a keybinding (`type = "plugin_action"`), or a link handler. |
| `[[panes]]` | `id`, `title`, `command`, optional `placement`, `platforms`, and `width`/`height` (popup only). |
| `[[events]]` | `on`, `command`. |
| `[[link_handlers]]` | `id`, `title`, `pattern` (Rust regex), `action`. |

Verified manifest (linked successfully, echoed back intact by `herdr plugin list --json`):

```toml
id = "probe.paneprobe"
name = "Pane Probe"
version = "0.1.0"
min_herdr_version = "0.7.0"
description = "Probes the pane entrypoint contract"
platforms = ["linux", "macos"]

[[panes]]
id = "probe"
title = "Pane Probe"
placement = "split"
command = ["sh", "probe.sh"]

[[actions]]
id = "probe-action"
title = "Probe action"
contexts = ["workspace"]
command = ["sh", "probe.sh"]
```

**`min_herdr_version` gating**: herdr refuses to link or install a plugin whose minimum is newer than the running binary (`plugin requires Herdr <version>`). Declare the oldest version supporting every API, event name, and manifest field used.

## Pane entrypoint — the important part

**[verified] A plugin pane is a real herdr pane running the command in a real PTY.** Probe observations:

- `tty` → `/dev/pts/1`; **stdin, stdout and stderr are all ttys** (`[ -t 0/1/2 ]` all true).
- `TERM=xterm-256color`; `stty size` → `52 153`.
- Alternate-screen + absolute cursor addressing (`\033[?1049h`, `\033[H`-style) rendered correctly → **a full-screen TUI works; ratatui/crossterm is viable as-is**.
- **`SIGWINCH` is delivered** on `herdr pane resize` and on `herdr pane zoom`, with `stty size` reporting the new geometry each time (`52 153` → `50 73` → `50 12` → `50 150`).
- **`herdr pane close <pane_id>` sends `SIGHUP`** to the process — the graceful-shutdown hook (flush cache, restore screen).
- **When the command exits, the pane disappears** from the snapshot. No lingering shell.
- **Working directory is the plugin root**, so `command = ["sh", "probe.sh"]` resolves relative to it — a relative path to a built Rust binary works the same way.

`placement` values: `overlay` (default — temporary zoomed overlay, restores prior focus on close), `split`, `tab`, `zoomed` (all three are normal herdr panes supporting `pane.move/swap/resize/zoom`), and `popup` (manifest only — a session-modal singleton with `width`/`height`, receives all terminal input, **has no pane id**, and emits no pane lifecycle events, so it does not participate in pane/layout/persistence/agent APIs).

`herdr plugin pane open` flags: `--plugin`, `--entrypoint`, `--placement {overlay|split|tab|zoomed}`, `--workspace`, `--target-pane`, `--direction {right|down}`, `--cwd`. The CLI exposes only the four non-popup placements; `plugin.pane.open` over the socket also accepts `popup`.

## Runtime environment

**[verified]** exactly this set was injected into a pane command:

```
HERDR_BIN_PATH=/home/nyaon/.local/bin/herdr
HERDR_ENV=1
HERDR_PANE_ID=w1:p4
HERDR_PLUGIN_CONFIG_DIR=/home/nyaon/.config/herdr/plugins/config/<plugin-id>
HERDR_PLUGIN_CONTEXT_JSON={...}
HERDR_PLUGIN_ENTRYPOINT_ID=probe
HERDR_PLUGIN_ID=probe.paneprobe
HERDR_PLUGIN_ROOT=<plugin dir>
HERDR_PLUGIN_STATE_DIR=/home/nyaon/.local/state/herdr/plugins/<plugin-id>
HERDR_SOCKET_PATH=/home/nyaon/.config/herdr/herdr.sock
HERDR_TAB_ID=w1:t3
HERDR_WORKSPACE_ID=w1
```

Docs add, per entrypoint kind: `HERDR_PLUGIN_ACTION_ID` (actions), `HERDR_PLUGIN_EVENT` + `HERDR_PLUGIN_EVENT_JSON` (events and startup), `HERDR_PLUGIN_CLICKED_URL` + `HERDR_PLUGIN_LINK_HANDLER_ID` (link handlers). `HERDR_PANE_ID` is absent for popups.

**[verified] `HERDR_PLUGIN_CONTEXT_JSON` as delivered to a pane:**

```json
{"workspace_id":"w1","workspace_label":"simata-start","workspace_cwd":"/home/nyaon/simata-start",
 "tab_id":"w1:t3","tab_label":"geometry edit guard","focused_pane_id":"w1:p3",
 "focused_pane_cwd":"/home/nyaon/simata-start","focused_pane_agent":"claude",
 "focused_pane_status":"working","invocation_source":"api","correlation_id":"plugin-pane"}
```

Note `workspace_cwd` and `focused_pane_cwd` — directly relevant to [How does a plugin learn the active workspace repo?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/4). Docs say action context can also carry worktree, agent, selected text, clicked URL and link-handler fields.

Storage rules: user-editable config → `HERDR_PLUGIN_CONFIG_DIR` (`herdr plugin config-dir <id>` prints it); runtime state → `HERDR_PLUGIN_STATE_DIR`. **Never** store credentials or durable state under `HERDR_PLUGIN_ROOT` — GitHub-installed roots are managed checkouts and are replaced on reinstall. There is no herdr-managed storage API in v1.

## Events

`[[events]] on = "<name>"` uses dotted names. The bundled schema defines **25** event types (snake_case in the API, dotted in the manifest):

`layout.updated`, `pane.agent_detected`, `pane.agent_status_changed`, `pane.closed`, `pane.created`, `pane.exited`, `pane.focused`, `pane.moved`, `pane.output_changed`, `pane.updated`, `tab.closed`, `tab.created`, `tab.focused`, `tab.moved`, `tab.renamed`, `workspace.closed`, `workspace.created`, `workspace.focused`, `workspace.metadata_updated`, `workspace.moved`, `workspace.renamed`, `workspace.updated`, `worktree.created`, `worktree.opened`, `worktree.removed`.

Handlers receive `HERDR_PLUGIN_EVENT` and `HERDR_PLUGIN_EVENT_JSON` plus the standard environment. `workspace.focused` is the obvious candidate for "the active repo changed".

## Link handlers

```toml
[[link_handlers]]
id = "github-issue"
title = "Open GitHub issue"
pattern = "^https://github\\.com/[^/]+/[^/]+/(issues|pull)/[0-9]+$"
action = "apply"
```

Modified-click (Control, all platforms) on a matching terminal URL routes to the named **action**, with `invocation_source = "link_click"`, `clicked_url` and `link_handler_id` in the context. Note the docs' own example is a GitHub issue URL — clicking an issue link in a Claude pane can open it in our viewer.

## Install, link, distribution

```bash
herdr plugin install <owner>/<repo>[/<subdir>] [--ref <ref>] [-y]   # runs [[build]], shows a preview first
herdr plugin link <path> [--enabled|--disabled]                     # local dev; skips [[build]]
herdr plugin unlink <id> | uninstall <id>
herdr plugin enable <id> | disable <id>
herdr plugin list [--plugin <id>] [--json]
herdr plugin config-dir <id>
herdr plugin action list|invoke ; herdr plugin pane open|focus|close ; herdr plugin log list
```

Install and link both work with no server running. Installing over a locally linked plugin is refused — unlink first. Reinstalling a GitHub plugin replaces the managed checkout. Distribution today is "install from a GitHub repo"; docs mention a marketplace listing "when launched".

## Trust model

No sandbox, no permission declarations. Build, startup, action, event and pane commands all run as the user with the user's environment; the install preview shows the source and the commands that will run so the user can review. Consequence for us: reading a GitHub token from the environment or from `gh` is unrestricted, and equally, users must trust the published plugin.

## What this pins down for the map

1. The plugin is a directory + `herdr-plugin.toml` + a Rust binary; no FFI, no plugin ABI, no herdr crate dependency.
2. The viewer is a **normal full-screen TUI** on a real PTY, with `SIGWINCH` for resize and `SIGHUP` for close. Standard `ratatui`/`crossterm` applies.
3. Declare `[[panes]] id = "issues"` with `placement = "split"`; the user opens it via keybinding → `plugin_action`, or `herdr plugin pane open`, or a `[[link_handlers]]` match on GitHub issue URLs.
4. Cache belongs in `HERDR_PLUGIN_STATE_DIR`, user config in `HERDR_PLUGIN_CONFIG_DIR`, never in the plugin root.
5. `min_herdr_version` must be declared; `0.7.0` is a safe floor for everything used above (verified working on 0.7.5).
6. Publishing means a GitHub repo with `[[build]]` commands — and a Rust `[[build]]` implies the installing user needs a toolchain, or the repo ships prebuilt binaries. That tension is [Packaging and distribution for a publishable Rust plugin](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/8)'s problem.

## Open questions this raised

- `[[build]]` runs on the **installing** user's machine, and herdr installs no toolchains — so a Rust plugin either demands `cargo` at install time or ships platform binaries fetched by the build command. → ticket 8.
- Pane input handling was not probed end-to-end (keystrokes into a focused plugin pane); the PTY makes it near-certain, but the UI prototype should confirm mouse and key handling. → [Pane UI shape](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/7).
- `herdr plugin log list` exists — what it captures for a long-lived pane process (stderr? exit codes?) is undocumented and unprobed.
