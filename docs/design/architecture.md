# Herdr Tab Auto-Renamer Architecture

Status: implemented design. ADR 0010 supersedes ADR 0009 while preserving focused-tab-only safety from ADR 0007.

## Goal

Build a Herdr plugin that automatically keeps tab labels meaningful. For the currently focused tab, the plugin inspects the tab's Focused Pane, prefers a stable Significant Command as the label, and falls back to the configured Working Directory Suffix when no useful command is present. Inactive tabs keep their last visible label until focused again so the tab bar stays stable while the user navigates.

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
   - fallback to Working Directory Suffix.
8. Pass candidates through stability checks:
   - require two consecutive observations before renaming;
   - keep the last Significant Command for a 2 second grace period before falling back to cwd;
   - skip no-op renames.
9. Detect and preserve Manually Locked Tabs.
10. Persist Automatic Rename Intent, revalidate, then apply at most one `tab.rename`.

## Rust module shape

Implemented files/modules in the Rust crate:

- `src/main.rs` — CLI entrypoint and command dispatch.
- `src/session_runtime.rs` — lifecycle, Startup Gate, lease, control ingress, scheduling, handoff, and effects.
- `src/refresh_decision.rs` — pure bounded One-Shot Refresh policy with no Herdr or persistence I/O.
- `src/refresh_executor.rs` — Herdr and Session-Scoped Tab State effect adapter for Refresh Decisions.
- `src/herdr_client.rs` — Herdr Unix-socket JSON-RPC client, DTOs, and Focused Observation/Process Inspector boundary.
- `src/config.rs` — resolves, parses, validates, and compiles versioned `config.toml` into one Label Policy.
- `src/labeler.rs` — Label Policy and candidate derivation.
- `src/stability.rs` — anti-flapping state machine.
- `src/locks.rs` — validated Session-Scoped Tab State and crash-safe rename reconciliation.
- `src/paths.rs` — plugin state/log paths.

Expected CLI/actions:

- `start` crosses the Startup Gate and activates its invoking registered binary, requesting Cooperative Runtime Handoff when a different validated binary is Ready;
- `ensure-started` and lifecycle hooks cross the Startup Gate non-destructively, signalling a Ready owner or recovering an absent owner;
- `refresh` signals the Ready owner;
- `install` relinks and ensures/hands off to the installed owner;
- `unlock-focused`, `unlock-all`, and `repair-state --discard` mutate through the owner;
- `forget-session` removes retained state only while the selected runtime is Absent.

## Refresh trigger model

Tabby prioritizes Navigation Stability while retaining five-second focused-tab freshness. Startup and focus hooks cross the gate and signal the owner; creation hooks recover a missing owner. Newer triggers invalidate unfinished generations. Client Detach does not affect the runtime, proven Session Stop releases ownership, and Session Restore starts a new owner for the same retained Session Identity.

## Manual lock semantics

Manual locks persist across plugin runs. Users unlock explicitly with `unlock-focused` or `unlock-all`; there is no implicit auto-unlock in v1. The Session Runtime respects persisted locks before inspecting panes or renaming.

## Distribution model

The primary release path is a GitHub-managed Herdr plugin. Its reviewed build command downloads the cargo-dist Apple Silicon archive and checksum, verifies them, and atomically prepares `.herdr/bin/tabby`. Homebrew remains an alternative adapter; `tabby install` relinks its packaged manifest and ensures that binary owns the selected Session Runtime through Cooperative Runtime Handoff when needed.

Local development remains a separate link flow:

```sh
cargo build
python3 scripts/prepare-herdr-plugin.py
herdr plugin link .
```

The production-shaped root manifest invokes `.herdr/bin/tabby` for both GitHub-managed installation and local linking; linked development checkouts prepare that path explicitly because Herdr does not run build commands for links. The Homebrew manifest invokes `../../bin/tabby` from the package share directory. Manifest and release-contract checks require identical behavior, aligned versions, the exact native archive/checksum pair, and checksum integrity. No path silently auto-updates.

## Test strategy

Use unit tests for the pure behavior first:

- label policy classification;
- Working Directory Suffix fallback;
- ignored shell/wrapper behavior;
- anti-flapping state transitions;
- manual lock detection;
- session-scoped state mutations over a temporary state directory.

The isolated macOS lifecycle harness adds real Herdr 0.8 evidence:

- default and named session startup through `[[startup]]`;
- concurrent hook coalescing and one Ready owner;
- Focus Quiet Window and periodic evaluation timing;
- crash recovery, Session Stop lease release, and Session Restore ownership;
- temporary HOME/XDG/config roots with no writes to real user configuration.

Deterministic Rust tests cover transport faults, disappearing targets, rename-intent reconciliation, corrupt state, handoff, state isolation, and Forget Session behavior. The recorded real-runtime evidence and its explicit limits live in `docs/evidence/issue-71-herdr-0.8-lifecycle.md`.
