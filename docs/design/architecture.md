# Herdr Tab Auto-Renamer Architecture

Status: implemented design. ADR 0010 supersedes ADR 0009 while preserving focused-tab-only safety from ADR 0007.

## Goal

Build a Herdr plugin that automatically keeps tab labels meaningful. For the currently focused tab, the plugin inspects the tab's Focused Pane, prefers a stable Significant Command as the label, and falls back to the Working Directory Basename when no useful command is present. Inactive tabs keep their last visible label until focused again so the tab bar stays stable while the user navigates.

## Inputs and controls

Primary Herdr APIs:

- `session.snapshot` — coherently observe workspaces, tabs, panes, and focus.
- `pane.process_info` — inspect foreground process details for app-first labels.
- `tab.rename` — apply a Stable Label Candidate to a tab.

Prior research lives in `docs/herdr-tab-title-research.md`. It is input, not final design.

## Core behavior

1. Run one lease-owned Session Runtime per Herdr Session through a session-scoped Startup Gate.
2. Receive `[[startup]]`, `pane.focused`, and creation-recovery hooks through a private authenticated control endpoint; do not open `events.subscribe`.
3. Reset a 1000 ms Focus Quiet Window on each actionable trigger; during the window, do not call any Herdr API.
4. Outside the quiet window, evaluate only the focused tab every 5 seconds; each attempt has a 2.5 second deadline, at most three samples, and 500 ms sample cadence.
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
10. Persist Automatic Rename Intent, revalidate, then apply at most one `tab.rename`.

## Rust module shape

Proposed files/modules for a single Rust crate:

- `src/main.rs` — CLI entrypoint and command dispatch.
- `src/session_runtime.rs` — lifecycle, Startup Gate, lease, control ingress, scheduling, handoff, and effects.
- `src/daemon.rs` — bounded One-Shot Refresh decision/execution.
- `src/herdr_client.rs` — Herdr Unix-socket JSON-RPC client and DTOs.
- `src/process_inspector.rs` — wrapper around `pane.process_info`; failure returns no Significant Command and allows cwd fallback.
- `src/labeler.rs` — Label Policy and candidate derivation.
- `src/stability.rs` — anti-flapping state machine.
- `src/locks.rs` — validated Session-Scoped Tab State and crash-safe rename reconciliation.
- `src/paths.rs` — plugin state/log paths.

Expected CLI/actions:

- `start` / `ensure-started` cross the Startup Gate;
- `refresh` signals the Ready owner;
- `install` relinks and ensures/hands off to the installed owner;
- `unlock-focused`, `unlock-all`, and `repair-state --discard` mutate through the owner;
- `forget-session` removes retained state only while the selected runtime is Absent.

## Refresh trigger model

Tabby prioritizes Navigation Stability while retaining five-second focused-tab freshness. Startup and focus hooks cross the gate and signal the owner; creation hooks recover a missing owner. Newer triggers invalidate unfinished generations. Client Detach does not affect the runtime, proven Session Stop releases ownership, and Session Restore starts a new owner for the same retained Session Identity.

## Manual lock semantics

Manual locks persist across plugin runs. Users unlock explicitly with `unlock-focused` or `unlock-all`; there is no implicit auto-unlock in v1. The Session Runtime respects persisted locks before inspecting panes or renaming.

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
- session-scoped state mutations over a temporary state directory.

Then add integration/manual verification against Herdr on macOS:

- focused pane behavior for inactive tabs;
- `pane.process_info` shape for `nvim`, `pnpm dev`, `lazygit`, `codex`, `claude`, `go test`;
- local `herdr plugin link .` startup behavior;
- no writes to real user config during automated validation.
