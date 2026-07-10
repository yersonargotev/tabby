# Tabby

Tabby is a Herdr plugin that keeps tab labels meaningful. A per-session Hybrid Session Refresher keeps the focused tab fresh, prefers stable foreground activity such as `nvim`, `codex`, or `pnpm dev`, and falls back to the working-directory basename when no significant command is running.

## Quick path

Install the packaged plugin through Homebrew:

```sh
brew install yersonargotev/tap/tabby
tabby install --start
```

Refresh the focused tab label manually when you want an immediate one-shot update:

```sh
tabby refresh
herdr plugin action invoke refresh --plugin yersonargotev.tabby
```

For the full install, verification, trust-model, uninstall, and rollback guide, see [`docs/install.md`](docs/install.md).

## What Tabby does

Tabby automatically renames Herdr tabs using this policy:

| Priority | Label source | Examples |
| --- | --- | --- |
| 1 | Significant Command | `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, `npm test`, `go test`, `cargo run` |
| 2 | Working Directory Basename | `/Users/me/dev/tabby` becomes `tabby` |

It also avoids common shell and wrapper processes such as `zsh`, `bash`, `tmux`, `env`, and `sudo`, so normal shell tabs still get useful directory labels.

## Behavior details

- Runs one Hybrid Session Refresher per Herdr Session for automatic focused-tab freshness.
- Starts idempotently through `tabby ensure-started`, `tabby install --start`, creation hooks, or the visible `Start Tabby` action.
- Verifies that an already-running refresher came from the current `tabby` executable. If live metadata points to a different binary (or cannot identify one), startup refuses instead of silently keeping stale local or Homebrew code active; stop the reported PID and rerun `tabby install --start`.
- Suppresses all Herdr API calls during the 1000 ms Focus Quiet Window after focus/create events.
- Inspects only the focused tab on a low-cadence 5 second idle interval outside the quiet window, and still requires two consecutive observations before new labels become stable.
- Leaves inactive tab labels unchanged so Tabby does not rewrite the tab bar while the user is navigating between tabs.
- Keeps `tabby refresh` as a safe one-shot manual recovery path.
- Treats user-edited labels as manual locks after Tabby has established a plugin label baseline, and persists those locks until an unlock action clears them.

Project vocabulary and domain rules live in [`CONTEXT.md`](CONTEXT.md).

## Commands

```text
Usage: tabby <refresh|start|ensure-started|install [--start]|unlock-focused|unlock-all>
```

| Command | Purpose |
| --- | --- |
| `tabby refresh` | Wait briefly, inspect the focused Herdr tab, apply at most one label refresh, and exit. |
| `tabby start` | Run the Hybrid Session Refresher in the foreground for the current Herdr Session. |
| `tabby ensure-started` | Ensure exactly one Hybrid Session Refresher is running for the current Herdr Session. |
| `tabby install` | Refresh Herdr registration for the current Homebrew-installed package; it does not start the refresher. |
| `tabby install --start` | Refresh registration and ensure the current Herdr Session Refresher is running. |
| `tabby unlock-focused` | Clear the manual lock and plugin-label baseline for the focused Herdr tab so automatic naming resumes. |
| `tabby unlock-all` | Clear all persisted manual locks and their associated plugin-label baselines so automatic naming resumes. |

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
git diff --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 scripts/check-herdr-manifests.py
cargo build
```

For release planning, also run:

```sh
dist plan
```

## Release notes

Tabby's v1 release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts and a Homebrew formula for Apple Silicon macOS. The release package installs a separate Herdr manifest at `share/tabby/herdr-plugin.toml` whose actions run the Homebrew-installed binary via `../../bin/tabby`. After install or upgrade, `tabby install` refreshes Herdr registration so stale Homebrew Cellar paths are replaced with the current package path. Automatic label updates come from the per-session Hybrid Session Refresher; creation hooks and the `Start Tabby` action run `ensure-started`, while `tabby refresh` remains the manual one-shot path.

Release setup and tagging details live in [`docs/release.md`](docs/release.md). The development and release manifests are kept aligned by [`scripts/check-herdr-manifests.py`](scripts/check-herdr-manifests.py).

## Documentation map

| File | Use |
| --- | --- |
| [`docs/install.md`](docs/install.md) | User install, verification, trust model, uninstall, and rollback. |
| [`docs/release.md`](docs/release.md) | Maintainer release process and required GitHub secret. |
| [`docs/design/architecture.md`](docs/design/architecture.md) | Architecture and module responsibilities. |
| [`docs/adr/`](docs/adr/) | Accepted architecture decisions. |
| [`docs/herdr-tab-title-research.md`](docs/herdr-tab-title-research.md) | Supporting research for Herdr tab-title behavior. |
