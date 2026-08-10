# Tabby

Tabby is a Herdr plugin that keeps the focused tab label meaningful within each Herdr Session. One session-owned runtime survives client detach, stops with the Herdr Session, and resumes on restore. It prefers stable foreground activity such as `nvim`, `codex`, or `pnpm dev`, then falls back to the working-directory basename.

## Quick path

Install the packaged plugin through Homebrew:

```sh
brew install yersonargotev/tap/tabby
tabby install
```

Request a focused-tab refresh manually:

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

- Runs one lease-owned Session Runtime per Herdr Session and starts it through Herdr 0.8 lifecycle/focus hooks or any manual action.
- Uses authenticated cooperative handoff after `tabby install`; it never treats PID metadata as ownership authority.
- Suppresses all Herdr API calls during the 1000 ms Focus Quiet Window after a Refresh Trigger.
- Re-evaluates only the focused tab every 5 seconds, with at most three samples at 500 ms cadence and two equal consecutive candidates before rename.
- Leaves inactive tab labels unchanged so Tabby does not rewrite the tab bar while the user is navigating between tabs.
- Routes refresh and unlock actions through the owner so effects remain serialized.
- Persists locks, baselines, and pre-rename intent by lossless Session Identity across stop/restore.

Project vocabulary and domain rules live in [`CONTEXT.md`](CONTEXT.md).

## Commands

```text
Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install|unlock-focused|unlock-all|repair-state --discard|forget-session>
```

| Command | Purpose |
| --- | --- |
| `tabby status` | Read authoritative runtime, session-state, registration, and focused-tab diagnostics without changing state. |
| `tabby refresh` | Deliver a manual Refresh Trigger to the Ready Session Runtime. |
| `tabby start` / `tabby ensure-started` | Cross the Startup Gate and ensure exactly one Ready Session Runtime. |
| `tabby install` | Refresh registration and cooperatively ensure the installed binary owns the current Herdr Session. |
| `tabby unlock-focused` | Clear the manual lock and plugin-label baseline for the focused Herdr tab so automatic naming resumes. |
| `tabby unlock-all` | Clear all persisted manual locks and their associated plugin-label baselines so automatic naming resumes. |
| `tabby repair-state --discard` | Explicitly archive invalid session-state evidence and create clean state. |
| `tabby forget-session` | Remove retained state for the selected stopped Herdr Session. |

Run `tabby status` first when labels are not updating. It reports Absent, Starting, Ready, or Faulted; lease ownership; runtime identity/version; the latest evaluation/failure and next periodic cycle; and per-session lock, baseline, and intent counts. The command is read-only.

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
python3 -m unittest discover -s scripts/tests
cargo build
```

On macOS with Herdr 0.8.0 installed, run the isolated real-lifecycle harness:

```sh
python3 scripts/herdr_lifecycle_harness.py
```

The harness uses temporary HOME/XDG/config roots, exercises default and named Herdr Sessions, writes a sanitized JSONL transcript under `.scratch/`, and removes its temporary sessions and state. It never falls back to the operator's Herdr configuration.

For release planning, also run:

```sh
dist plan
```

## Release notes

Tabby's v1 release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts and a Homebrew formula for Apple Silicon macOS. The release package installs a separate Herdr manifest at `share/tabby/herdr-plugin.toml` whose actions run the Homebrew-installed binary via `../../bin/tabby`. After install or upgrade, `tabby install` refreshes registration and performs a cooperative Session Runtime handoff when needed.

Release setup and tagging details live in [`docs/release.md`](docs/release.md). The development and release manifests are kept aligned by [`scripts/check-herdr-manifests.py`](scripts/check-herdr-manifests.py).

## Documentation map

| File | Use |
| --- | --- |
| [`docs/install.md`](docs/install.md) | User install, verification, trust model, uninstall, and rollback. |
| [`docs/release.md`](docs/release.md) | Maintainer release process and required GitHub secret. |
| [`docs/design/architecture.md`](docs/design/architecture.md) | Architecture and module responsibilities. |
| [`docs/evidence/issue-71-herdr-0.8-lifecycle.md`](docs/evidence/issue-71-herdr-0.8-lifecycle.md) | Recorded real-Herdr 0.8 lifecycle evidence and coverage limits. |
| [`docs/adr/`](docs/adr/) | Accepted architecture decisions. |
| [`docs/herdr-tab-title-research.md`](docs/herdr-tab-title-research.md) | Historical research that preceded the implemented Session Runtime. |
