# ADR-0003: Fetch-or-build install, tag-driven releases for four targets

- **Status**: accepted
- **Date**: 2026-07-27
- **Ticket**: [Packaging and distribution for a publishable Rust plugin](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/8)

## Context

The plugin ships to other herdr users. Two facts make packaging load-bearing rather than incidental:

- **[verified]** `[[build]]` commands run on the **installing user's machine**, after confirmation, and only on `herdr plugin install` — never on `herdr plugin link`. Herdr reports a build failure but installs no toolchains ([`herdr-plugin-contract.md`](../research/herdr-plugin-contract.md)).
- The cache is SQLite ([ADR-0001](./0001-issue-cache-sqlite.md)), and `rusqlite` with `bundled` compiles SQLite from C. So a naive `cargo build` install demands both a Rust toolchain **and** a C compiler from every user.

Prior art was surveyed among Rust plugins already listed on the marketplace:

- `smarzban/herdr-file-viewer` (259★, 20 releases) — `[[build]] command = ["/bin/sh", "scripts/fetch-or-build.sh"]`, downloading a prebuilt matching the source version and platform, verifying it, falling back to a build; a separate PowerShell script for Windows.
- `persiyanov/herdr-reviewr` (253★, 30 releases) — `["bash", "herdr/install.sh"]`, downloading a prebuilt; pane runs the binary by absolute path under the plugin root.
- `yuk1ty/herdr-spreader` — plain `["cargo", "build", "--release"]` with the pane command `./target/release/herdr-spreader`.

The marketplace itself is live and trivial: an unreviewed index of public repos carrying the GitHub topic `herdr-plugin`, refreshed every 30 minutes, forks and archived repos excluded. It shows repo name, owner, description, stars, language and last push — manifest fields like id, platforms and `min_herdr_version` are not surfaced in v1.

## Decision

### Install: fetch-or-build

```toml
[[build]]
platforms = ["linux", "macos"]
command = ["/bin/sh", "scripts/fetch-or-build.sh"]
```

The script reads `version` from `herdr-plugin.toml`, downloads `herdr-issues-<target>.tar.gz` from the matching GitHub Release, verifies it against `checksums.txt`, unpacks to `./bin/herdr-issues`, and falls back to `cargo build --release` on any failure — no release for this platform, no network, bad checksum.

Installs take seconds and need neither Rust nor a C compiler, which is what makes bundled SQLite acceptable. The fallback keeps unbuilt targets working rather than failing hard.

`[[panes]] command = ["./bin/herdr-issues"]`, resolved against the plugin root, which is the pane's working directory.

### Targets: Linux musl and macOS, both arches

| artifact |
|---|
| `herdr-issues-x86_64-unknown-linux-musl.tar.gz` |
| `herdr-issues-aarch64-unknown-linux-musl.tar.gz` |
| `herdr-issues-x86_64-apple-darwin.tar.gz` |
| `herdr-issues-aarch64-apple-darwin.tar.gz` |
| `checksums.txt` |

`platforms = ["linux", "macos"]` in the manifest. musl gives one fully static binary per arch, indifferent to the distro's glibc and fine under WSL; `rustls` (no OpenSSL) and bundled SQLite make static linking painless. Windows is out for v1 — it doubles the CI matrix and the script surface for a platform that is neither used nor testable here.

### Repo layout and releases

`herdr-plugin.toml` sits at the **repo root**, so `herdr plugin install nyanyaon/github-issue-herdr-plugin` needs no subdir path.

```
herdr-plugin.toml          version = "0.1.0", min_herdr_version = "0.7.0"
Cargo.toml
src/
scripts/fetch-or-build.sh
.github/workflows/release.yml    on: push tags 'v*'
docs/adr/
```

Pushing a `v0.1.0` tag builds all four targets, writes `checksums.txt`, and creates the release. **CI fails if the tag and the manifest `version` disagree** — the fetch script keys off the manifest version, so a mismatch would silently send every installer down the slow path.

`min_herdr_version = "0.7.0"`: verified sufficient for every API used, and the floor all three surveyed Rust plugins declare.

Marketplace listing is one step: keep the repo public and add the topic `herdr-plugin`.

### Configuration

`$HERDR_PLUGIN_CONFIG_DIR/config.toml`, **absent by default and never required** — a fresh install works with zero configuration. Every key optional:

```toml
list_page_size = 50
detail_comment_page_size = 100
prune_details_after_days = 30
prune_repos_after_days = 90

[repo."/home/me/work/fork"]
slug = "upstream-org/project"
```

The per-repo table is where overrides land for the cases [Repo identity: which remote names the repo?](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/10) defines; the token source key belongs to [Token discovery and API failure policy](https://github.com/nyanyaon/github-issue-herdr-plugin/issues/11). Theme and keymap are deliberately not configurable in v1.

## Consequences

- A release is not optional infrastructure: without a published artifact for a platform, every install there silently becomes a multi-minute compile. CI is the product surface.
- The plugin `id` is permanent in practice — it names `HERDR_PLUGIN_CONFIG_DIR` and `HERDR_PLUGIN_STATE_DIR`, so changing it later orphans user config and cache. It must be settled before the first release; `nyanyaon.herdr-issues` is the working proposal.
- `checksums.txt` makes the install verifiable, but the trust model is still "code runs as your user" — herdr sandboxes nothing, and the install preview showing build commands is the only review step.
- Windows users get whatever `cargo build` gives them, on a TUI that has never been tested there. The manifest's `platforms` says linux and macos, so that is honest rather than silent.
- The `[[build]]`/`plugin link` split means local development never runs the fetch script — the dev loop is `cargo build --release` plus `herdr plugin link`, and the fetch path only ever exercises in CI or a real install.

## Alternatives rejected

- **`cargo build --release` only** — simplest manifest, no release pipeline, but demands a Rust toolchain and a C compiler from every user plus a cold compile at install.
- **Prebuilt only, no fallback** — predictable when it works, a hard wall on any unpublished target.
- **Adding Windows** — doubles CI and adds a PowerShell fetch script for an untested platform.
- **Two artifacts only (linux x86_64, macOS arm64)** — cheapest release, but silently slow installs on Apple Intel and ARM Linux.
- **Plugin in a subdirectory** — justified only if the repo held more than this plugin.
- **Hand-built releases** — macOS arm64 and Linux musl cross-builds from one machine are the tedium CI exists to remove.
- **Env-vars-only configuration** — leaves per-repo overrides homeless.
- **Full theme/keymap config** — a schema to keep stable forever, before anyone has used the thing.
