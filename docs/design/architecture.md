# Herdr Tab Auto-Renamer Architecture Proposal

Status: initial design, no implementation yet.

## Goal

Build a Herdr plugin that automatically keeps tab labels meaningful. For each tab, the plugin inspects the tab's Focused Pane, prefers a stable Significant Command as the label, and falls back to the Working Directory Basename when no useful command is present.

## Inputs and controls

Primary Herdr APIs:

- `tab.list` — enumerate tabs and current labels.
- `pane.list` — enumerate panes, tab ownership, focus state, `cwd`, and `foreground_cwd` when available.
- `pane.process_info` — inspect foreground process details for app-first labels.
- `tab.rename` — apply a Stable Label Candidate to a tab.

Prior research lives in `docs/herdr-tab-title-research.md`. It is input, not final design.

## Core behavior

1. Poll Herdr state every 500 ms.
2. For each tab, select its Focused Pane.
3. Ask the Process Inspector for foreground process details for that pane.
4. Use Label Policy to derive a Tab Label Candidate:
   - known interactive apps: `nvim`, `lazygit`, `codex`, `claude`;
   - useful runner/subcommand pairs: `pnpm dev`, `npm test`, `go test`, `cargo run`;
   - ignore shells, opaque wrappers, and transient processes;
   - fallback to Working Directory Basename.
5. Pass candidates through stability checks:
   - require two consecutive observations before renaming;
   - keep the last Significant Command for a 2 second grace period before falling back to cwd;
   - skip no-op renames.
6. Detect and preserve Manually Locked Tabs.
7. Rename only unlocked tabs with stable labels.

## Rust module shape

Proposed files/modules for a single Rust crate:

- `src/main.rs` — CLI entrypoint and command dispatch.
- `src/daemon.rs` — daemon loop and orchestration.
- `src/herdr_client.rs` — Herdr Unix-socket JSON-RPC client and DTOs.
- `src/process_inspector.rs` — wrapper around `pane.process_info`; failure returns no Significant Command and allows cwd fallback.
- `src/labeler.rs` — Label Policy and candidate derivation.
- `src/stability.rs` — anti-flapping state machine.
- `src/locks.rs` — persisted Manually Locked Tab store.
- `src/paths.rs` — plugin state/log paths.

Expected CLI/actions:

- daemon/start command for Herdr autostart;
- `ensure-started` command as the idempotent startup boundary for install, hooks, and the user-facing Herdr `start` action;
- `install --start` for explicit current-session startup after relinking;
- `unlock-focused` to remove the focused tab from the persisted lock store;
- `unlock-all` to clear all persisted manual locks.

## Session startup model

Tabby runs one Tabby Session Daemon per Herdr Session/socket. Plain `tabby install` only relinks/registers the Homebrew-managed plugin and must not silently launch a long-running process. Users who want immediate current-session activation use `tabby install --start`, which delegates to `tabby ensure-started`.

`ensure-started` is the only normal startup boundary. It derives a per-socket session key, acquires plugin-owned daemon state under `daemons/<session_key>.lock`, validates any existing `daemons/<session_key>.json` metadata, and spawns detached `tabby start` only when no matching live daemon exists. The lower-level `tabby start` command still runs the daemon loop, but Herdr manifests should not invoke it directly.

Herdr manifests should keep a single user-facing `start` action id for compatibility and recovery, but that action should run `ensure-started`. Initial lifecycle hooks are `workspace.created` and `tab.created`, both also running `ensure-started`. `pane.created` and focus hooks are deferred unless real Herdr verification proves the initial hook set does not start Tabby early enough.

Herdr 0.7.3 has no documented session-start/autostart hook, so restored sessions may not start Tabby until a supported creation hook fires or the user explicitly starts it. That limitation is intentional and documented rather than hidden behind noisy focus hooks.

## Manual lock semantics

A tab becomes Manually Locked when its current label changes to a value that is neither the plugin's last-applied/last-seen label nor the current Stable Label Candidate. Locks persist across daemon/plugin restarts. Users unlock explicitly with `unlock-focused` or `unlock-all`; there is no implicit auto-unlock in v1.

## Distribution model

V1 is local-link only:

```sh
cargo build
herdr plugin link .
```

Release/install packaging is intentionally deferred but important. Before broader distribution, add reproducible release builds, macOS binaries first, checksums, and an auditable install script. No silent auto-updates.

## Test strategy

Use unit tests for the pure behavior first:

- label policy classification;
- cwd basename fallback;
- ignored shell/wrapper behavior;
- anti-flapping state transitions;
- manual lock detection;
- unlock actions over a temporary lock store.

Then add integration/manual verification against Herdr on macOS:

- focused pane behavior for inactive tabs;
- `pane.process_info` shape for `nvim`, `pnpm dev`, `lazygit`, `codex`, `claude`, `go test`;
- local `herdr plugin link .` startup behavior;
- no writes to real user config during automated validation.
