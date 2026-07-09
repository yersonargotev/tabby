# Herdr Tab Auto-Renamer Architecture Proposal

Status: implemented design; ADR 0008 supersedes the earlier polling-daemon startup model.

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

1. Run only from an accepted Refresh Trigger: workspace created/focused, tab created/focused, or manual refresh.
2. Wait briefly for focus/process state to settle.
3. Read the tab focused at refresh time; inactive tabs are not inspected or renamed.
4. Select the focused tab's Focused Pane.
5. Ask the Process Inspector for foreground process details for that pane.
6. Use Label Policy to derive a Tab Label Candidate:
   - known interactive apps: `nvim`, `lazygit`, `codex`, `claude`;
   - useful runner/subcommand pairs: `pnpm dev`, `npm test`, `go test`, `cargo run`;
   - ignore shells, opaque wrappers, and transient processes;
   - fallback to Working Directory Basename.
6. Pass candidates through stability checks:
   - require two consecutive observations before renaming;
   - keep the last Significant Command for a 2 second grace period before falling back to cwd;
   - skip no-op renames.
7. Detect and preserve Manually Locked Tabs.
8. Apply at most one `tab.rename` to the focused unlocked tab, then exit.

## Rust module shape

Proposed files/modules for a single Rust crate:

- `src/main.rs` — CLI entrypoint and command dispatch.
- `src/daemon.rs` — one-shot refresh orchestration plus legacy daemon-loop internals kept out of normal CLI paths.
- `src/herdr_client.rs` — Herdr Unix-socket JSON-RPC client and DTOs.
- `src/process_inspector.rs` — wrapper around `pane.process_info`; failure returns no Significant Command and allows cwd fallback.
- `src/labeler.rs` — Label Policy and candidate derivation.
- `src/stability.rs` — anti-flapping state machine.
- `src/locks.rs` — persisted Manually Locked Tab store.
- `src/paths.rs` — plugin state/log paths.

Expected CLI/actions:

- `refresh` for a One-Shot Refresh after a manual action or accepted Herdr trigger;
- `install` to relink/register the Homebrew-managed plugin without launching a long-running process;
- `unlock-focused` to remove the focused tab from the persisted lock store;
- `unlock-all` to clear all persisted manual locks.

## Refresh trigger model

Tabby prioritizes Navigation Stability over label freshness. It no longer keeps a continuously polling Tabby Session Daemon as the normal update path.

Accepted Refresh Triggers are `workspace.created`, `workspace.focused`, `tab.created`, `tab.focused`, and the manual `Refresh Tabby Label` action. Each trigger runs `tabby refresh`, waits briefly for focus/process state to settle, selects the tab focused at refresh time, inspects only that tab's selected pane, applies at most one `tab.rename`, and exits. Pane output changes, layout updates, and continuous polling are intentionally not Refresh Triggers.

`tabby install` only refreshes Herdr registration. It does not start daemons and does not clean up stale daemon metadata from older releases; old local daemon cleanup is an operator verification step, not product behavior.

## Manual lock semantics

Manual locks persist across plugin runs. Users unlock explicitly with `unlock-focused` or `unlock-all`; there is no implicit auto-unlock in v1. One-shot refreshes respect persisted locks before inspecting panes or renaming.

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
