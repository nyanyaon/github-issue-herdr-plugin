# GitHub Issues — a herdr pane

Read the issues of the repo you are working in, in a herdr pane, without opening
a browser tab.

The pane shows the open issues of the repo behind the current workspace; `enter`
opens one and renders its body and comment thread as terminal text. It is
**read-only** — it never comments, labels, assigns or closes anything, and it
issues no mutating GitHub request at all.

It costs nothing while idle: no browser, no polling, no timers, no background
process. Data comes from one small GraphQL query, is cached in SQLite, and
refreshes only when you press `r`. Opening the pane on a repo you have already
read is instant and needs no network.

---

## Install

```sh
herdr plugin install nyanyaon/github-issue-herdr-plugin
```

Herdr shows an install preview listing the commands that will run, and installs
nothing until you confirm.

What that install does:

1. Reads the `version` from `herdr-plugin.toml` and detects your target from
   `uname`.
2. Downloads `herdr-issues-<target>.tar.gz` from the matching GitHub Release.
3. **Verifies it against `checksums.txt` before unpacking it.** An archive whose
   SHA-256 does not match is deleted unread and never placed or run.
4. Unpacks the binary to `bin/herdr-issues` inside the plugin directory, which is
   where the manifest's pane entrypoint expects it.

On **any** failure — no release for your platform, no network, a checksum
mismatch, an unrecognised target — it falls back to `cargo build --release`, so
an unpublished platform still ends up with a working plugin. The only way an
install can fail outright is a fallback with no Rust toolchain installed, and it
says so.

The script is [`scripts/fetch-or-build.sh`](./scripts/fetch-or-build.sh); it runs
on `herdr plugin install` only, never on `herdr plugin link`.

---

## Open the pane, and bind a key

Open it once:

```sh
herdr plugin pane open --plugin nyanyaon.herdr-issues --entrypoint issues
```

To bind a key, add this to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+i"
type = "plugin_action"
command = "nyanyaon.herdr-issues.open"
description = "GitHub issues"
```

Then `herdr server reload-config`, and `ctrl+b i` opens the pane.

A `plugin_action` binding resolves an **action** id, not a pane id, so the
manifest declares one action, `open`, whose only job is to ask herdr to open the
pane above. `nyanyaon.herdr-issues.open` is `<plugin id>.<action id>`.

The pane opens as a split. Every key it binds is a plain key — herdr keeps its
own bindings behind `ctrl+b`, and this plugin binds neither `ctrl+b` nor
`ctrl+v`.

### Keys in the pane

**List**

| key | does |
|---|---|
| `j` / `k`, arrows | move |
| `g` / `G` | top / bottom |
| `enter` | open the issue |
| `/` | filter as you type — number, title, label and author. No network. |
| `o` | cycle open → closed → all |
| `r` | refresh the list |
| `q` | close the pane |

**Detail**

| key | does |
|---|---|
| `esc` | back to the list, position intact |
| `j` / `k` | scroll |
| `n` / `p` | next / previous issue without going back |
| `m` | load more comments |
| `r` | re-fetch this issue |
| `q` | close the pane |

A `●` on a list row means the issue has changed since the copy in the cache; it
re-fetches when you open it, not before.

---

## The GitHub token

GitHub's GraphQL API requires authentication — there is no anonymous mode — so
the pane needs a token. It reads one from the first of these that yields
anything, in this order:

1. **`$GITHUB_TOKEN`**
2. **`$GH_TOKEN`**
3. **`gh auth token`** — if you already use the `gh` CLI, this is zero setup.
4. **`token_file`** in `config.toml` — a path to a file whose first line is the
   token, trimmed.
5. Otherwise the pane says so on its status line and shows nothing else.

The plugin **never writes a credential**. It has no OAuth app, no token of its
own, and nothing for you to rotate — every token above is one you already own and
manage elsewhere. `token_file` is read, never written.

The token is resolved once, at startup. If it expires mid-session you will see
`token rejected`; reopen the pane after fixing it.

### What the token needs to be allowed to do

A read-only viewer should never hold write access. Use a **fine-grained personal
access token** with, on the repositories you want to read:

| permission | access |
|---|---|
| **Issues** | **Read-only** |
| **Metadata** | **Read-only** (required alongside Issues) |

Nothing else. No `contents`, no `pull requests`, no write anywhere.

**Fallback**, for organisations that have not enabled fine-grained tokens, or if
you just ran `gh auth login`: a **classic** PAT with `public_repo` for public
repositories, or `repo` for private ones. `repo` is read *and write* on
everything, which is far more than this plugin uses — prefer the fine-grained
token where you can.

---

## Configuration

Optional. A fresh install needs none. If you want it, create
`config.toml` in the plugin's config directory:

```sh
herdr plugin config-dir nyanyaon.herdr-issues
```

```toml
list_page_size = 50                   # issues fetched per list query
detail_comment_page_size = 100        # comments per detail page (GraphQL max)
prune_details_after_days = 30
prune_repos_after_days = 90
token_file = "/home/me/.secrets/gh-issues"

# Override the repo for one checkout, keyed by its repo root exactly. For a fork
# whose `origin` is not the repo whose issues you want to read, or a checkout
# with no remote at all.
[repo."/home/me/work/fork"]
slug = "upstream-org/project"
```

Every key is optional. An unknown key warns and is ignored; a malformed file
falls back to the defaults rather than refusing to start.

Without an override, the repo is resolved from the workspace's repo root:
`origin` first, then the sole remote if there is exactly one, otherwise the pane
lists the candidates and asks you to set `slug`. Only `github.com` is supported.
Every worktree of a repo shows the same list.

---

## Platform support

| platform | how you get the binary |
|---|---|
| Linux x86-64 | prebuilt, `x86_64-unknown-linux-musl` |
| Linux arm64 | prebuilt, `aarch64-unknown-linux-musl` |
| macOS Intel | prebuilt, `x86_64-apple-darwin` |
| macOS Apple silicon | prebuilt, `aarch64-apple-darwin` |
| Windows, anything else | source build, untested |

The Linux builds are static musl binaries, so they do not care what libc your
distribution ships, and they work under WSL.

**Why prebuilt binaries rather than just building on install**: the cache is
SQLite, and `rusqlite` is vendored with the `bundled` feature — it compiles
SQLite from C. A source build therefore needs a **C compiler** as well as a Rust
toolchain, and herdr installs neither. Downloading a verified prebuilt is what
keeps that off your machine and the install down to seconds.

Windows is not a declared platform. `herdr-plugin.toml` says
`platforms = ["linux", "macos"]`, which is honest rather than silent: a Windows
install falls through to `cargo build --release` and gets a TUI nobody has tested
there.

---

## What it deliberately does not do

- **Write anything.** No comments, labels, assignees or state changes.
- **Poll.** Nothing fetches unless you press `r`. Nothing retries by itself.
- **Render images**, or align tables — those degrade to their markdown source.
- **Pull requests**, a cross-repo inbox, or GitHub Enterprise.

---

## Development

`herdr plugin link` never runs the install script, so the dev loop builds the
binary itself:

```sh
cargo build --release
install -D target/release/herdr-issues bin/herdr-issues
herdr plugin link .
```

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Releasing is one command — push a `v<version>` tag matching the manifest.
[`.github/workflows/release.yml`](./.github/workflows/release.yml) builds all
four targets, writes `checksums.txt` and creates the release, and fails before
building anything if the tag and `herdr-plugin.toml` disagree about the version.

The design lives in [`CONTEXT.md`](./CONTEXT.md), [`docs/SPEC.md`](./docs/SPEC.md)
and [`docs/adr/`](./docs/adr/).

## Licence

MIT.
