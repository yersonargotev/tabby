# Tabby

Tabby is a Herdr plugin that keeps tab labels meaningful. It watches Herdr tabs, prefers stable foreground activity such as `nvim`, `codex`, or `pnpm dev`, and falls back to the working-directory basename when no significant command is running.

## Quick path

Install the packaged plugin through Homebrew:

```sh
brew install yersonargotev/tap/tabby
tabby install
```

Start the Tabby Session Daemon for the current Herdr Session explicitly:

```sh
tabby install --start
herdr plugin action invoke start --plugin yersonargotev.tabby
```

For the full install, verification, trust-model, stop, uninstall, and rollback guide, see [`docs/install.md`](docs/install.md).

## What Tabby does

Tabby automatically renames Herdr tabs using this policy:

| Priority | Label source | Examples |
| --- | --- | --- |
| 1 | Significant Command | `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, `npm test`, `go test`, `cargo run` |
| 2 | Working Directory Basename | `/Users/me/dev/tabby` becomes `tabby` |

It also avoids common shell and wrapper processes such as `zsh`, `bash`, `tmux`, `env`, and `sudo`, so normal shell tabs still get useful directory labels.

## Behavior details

- Polls Herdr state and renames the focused unlocked tab when the candidate label is stable.
- Leaves inactive tab labels unchanged so Tabby does not rewrite the tab bar while the user is navigating between tabs.
- Requires repeated observations before applying a label to reduce flapping.
- Keeps the last Significant Command briefly before falling back to a cwd label.
- Treats user-edited tab labels as Manually Locked Tabs.
- Persists manual locks across daemon restarts until an unlock action clears them.

Project vocabulary and domain rules live in [`CONTEXT.md`](CONTEXT.md).

## Commands

```text
Usage: tabby <daemon|start|ensure-started|install [--start]|unlock-focused|unlock-all>
```

| Command | Purpose |
| --- | --- |
| `tabby start` | Low-level command that runs the long-running Herdr rename loop. |
| `tabby daemon` | Alias for the same low-level daemon loop. |
| `tabby ensure-started` | Idempotently start one Tabby Session Daemon for the current Herdr Session. |
| `tabby install` | Refresh Herdr registration for the current Homebrew-installed package without starting a daemon. |
| `tabby install --start` | Refresh registration, then idempotently start the Tabby Session Daemon for the current Herdr Session. |
| `tabby unlock-focused` | Clear the manual lock for the focused Herdr tab. |
| `tabby unlock-all` | Clear all persisted manual locks. |

## Local development

Build the local debug binary and link this checkout as a Herdr plugin:

```sh
cargo build
herdr plugin link .
```

The root [`herdr-plugin.toml`](herdr-plugin.toml) is the local development manifest. Its actions invoke `target/debug/tabby`, so rebuild after code changes before testing through Herdr.

## Verification

Run the focused local checks before opening a PR:

```sh
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 scripts/check-herdr-manifests.py
```

For release planning, also run:

```sh
dist plan
```

## Release notes

Tabby's v1 release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts and a Homebrew formula for Apple Silicon macOS. The release package installs a separate Herdr manifest at `share/tabby/herdr-plugin.toml` whose actions run the Homebrew-installed binary via `../../bin/tabby`. After install or upgrade, `tabby install` refreshes Herdr registration so stale Homebrew Cellar paths are replaced with the current package path; use `tabby install --start` when you also want to start the Tabby Session Daemon for the current Herdr Session.

Release setup and tagging details live in [`docs/release.md`](docs/release.md). The development and release manifests are kept aligned by [`scripts/check-herdr-manifests.py`](scripts/check-herdr-manifests.py).

## Documentation map

| File | Use |
| --- | --- |
| [`docs/install.md`](docs/install.md) | User install, verification, trust model, stop, uninstall, and rollback. |
| [`docs/release.md`](docs/release.md) | Maintainer release process and required GitHub secret. |
| [`docs/design/architecture.md`](docs/design/architecture.md) | Architecture and module responsibilities. |
| [`docs/adr/`](docs/adr/) | Accepted architecture decisions. |
| [`docs/herdr-tab-title-research.md`](docs/herdr-tab-title-research.md) | Supporting research for Herdr tab-title behavior. |
