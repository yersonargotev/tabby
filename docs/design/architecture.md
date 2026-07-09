# Herdr Tab Auto-Renamer Architecture Proposal

Status: implemented design; ADR 0009 supersedes the one-shot-only model from ADR 0008 while preserving focused-tab-only safety from ADR 0007.

## Goal

Build a Herdr plugin that automatically keeps tab labels meaningful. For the currently focused tab, the plugin inspects the tab's Focused Pane, prefers a stable Significant Command as the label, and falls back to the Working Directory Basename when no useful command is present. Inactive tabs keep their last visible label until focused again so the tab bar stays stable while the user navigates.

## Inputs and controls

Primary Herdr APIs:

- `tab.list` — enumerate tabs and current labels.
- `pane.list` — enumerate panes, tab ownership, focus state, `cwd`, and `foreground_cwd` when available.
- `pane.process_info` — inspect foreground process details for app-first labels.
- `tab.rename` — apply a Stable Label Candidate to a tab.

Prior research lives in `docs/herdr-tab-title-research.md`. It is input, not final design.

## Core behavior

1. Run one Hybrid Session Refresher per Herdr Session, started idempotently by `ensure-started`.
2. Subscribe to `tab.focused`, `workspace.focused`, `tab.created`, `workspace.created`, and `pane.focused`.
3. Reset a 1000 ms Focus Quiet Window on each focus/create event; during the window, do not call `pane.process_info` or `tab.rename`.
4. Outside the quiet window, read only the focused tab; inactive tabs are not inspected or renamed.
5. Select the focused tab's Focused Pane.
6. Ask the Process Inspector for foreground process details for that pane.
7. Use Label Policy to derive a Tab Label Candidate:
   - known interactive apps: `nvim`, `lazygit`, `codex`, `claude`;
   - useful runner/subcommand pairs: `pnpm dev`, `npm test`, `go test`, `cargo run`;
   - ignore shells, opaque wrappers, and transient processes;
   - fallback to Working Directory Basename.
8. Pass candidates through stability checks:
   - require two consecutive observations before renaming;
   - keep the last Significant Command for a 2 second grace period before falling back to cwd;
   - skip no-op renames.
9. Detect and preserve Manually Locked Tabs.
10. Apply `tab.rename` only after the Focus Quiet Window and stability/revalidation gates pass.

## Rust module shape

Proposed files/modules for a single Rust crate:

- `src/main.rs` — CLI entrypoint and command dispatch.
- `src/daemon.rs` — Hybrid Session Refresher loop, one-shot refresh compatibility path, focused-tab inspection, and lock-aware rename orchestration.
- `src/herdr_client.rs` — Herdr Unix-socket JSON-RPC client and DTOs.
- `src/process_inspector.rs` — wrapper around `pane.process_info`; failure returns no Significant Command and allows cwd fallback.
- `src/labeler.rs` — Label Policy and candidate derivation.
- `src/stability.rs` — anti-flapping state machine.
- `src/locks.rs` — persisted Manually Locked Tab store.
- `src/paths.rs` — plugin state/log paths.

Expected CLI/actions:

- `start` to run the Hybrid Session Refresher in the foreground;
- `ensure-started` to idempotently start one refresher for the current Herdr Session;
- `refresh` for a manual one-shot refresh without refresher IPC;
- `install` to relink/register the Homebrew-managed plugin without launching a long-running process, plus `install --start` to also ensure the refresher;
- `unlock-focused` to remove the focused tab from the persisted lock store;
- `unlock-all` to clear all persisted manual locks.

## Refresh trigger model

Tabby prioritizes Navigation Stability while restoring automatic label freshness. The normal update path is one Hybrid Session Refresher per Herdr Session. Manifest startup hooks are limited to `workspace.created` and `tab.created`, both running `ensure-started`; focus events are handled inside the live refresher instead of spawning processes.

The refresher subscribes to `workspace.focused`, `tab.focused`, `pane.focused`, `workspace.created`, and `tab.created`. Every such event resets the 1000 ms Focus Quiet Window. Pane output changes and layout updates remain intentionally out of scope.

`tabby install` only refreshes Herdr registration. `tabby install --start` refreshes registration and ensures the current Herdr Session refresher is running.

## Manual lock semantics

Manual locks persist across plugin runs. Users unlock explicitly with `unlock-focused` or `unlock-all`; there is no implicit auto-unlock in v1. The refresher and one-shot refresh path both respect persisted locks before inspecting panes or renaming.

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
