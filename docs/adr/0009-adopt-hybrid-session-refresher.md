# Adopt Hybrid Session Refresher

Tabby will replace the one-shot-only default from ADR 0008 with a Hybrid Session Refresher: one long-running process per Herdr Session that restores automatic label freshness while keeping the navigation protections learned from the click-interference bug.

Status: Accepted. Supersedes ADR 0008 for normal automatic behavior. Preserves ADR 0007's focused-tab-only rule.

## Context

ADR 0008 fixed the immediate navigation problem by removing the continuously polling daemon and using narrow One-Shot Refresh triggers. Real use showed that this protected mouse navigation, but the label freshness trade-off was too severe: labels no longer update while the user remains in the same tab and foreground activity changes.

The earlier daemon model also cannot be restored unchanged. v0.1.6 already skipped inactive tabs, but still left recurring Herdr API activity and occasional focused-tab `tab.rename` calls while the user was navigating. The new design must make mouse tab navigation a first-class invariant, not a best-effort side effect.

## Decision

Tabby will run a Hybrid Session Refresher with these rules:

- `tabby start` runs the refresher in the foreground for the current Herdr Session.
- `tabby ensure-started` idempotently ensures exactly one refresher per Herdr Session.
- `tabby install --start` registers the plugin and ensures the refresher for the current Herdr Session.
- Plain `tabby install` remains registration-only.
- The visible Herdr startup action is `start` / `Start Tabby`, and it executes `tabby ensure-started`.
- Manifest auto-start hooks are limited to `workspace.created` and `tab.created`; focus events must not spawn or ensure processes.
- The refresher subscribes to focus/create events over the Herdr socket: `tab.focused`, `workspace.focused`, `tab.created`, `workspace.created`, and `pane.focused`.
- Every focus/create event resets a 1000 ms Focus Quiet Window.
- During the Focus Quiet Window, the refresher must not call `tab.rename` or `pane.process_info`; only minimal focus-state reads are allowed.
- Outside the quiet window, the refresher may inspect the focused tab every 500 ms and uses the existing two-consecutive-observation stability requirement.
- The refresher never inspects processes or renames Inactive Tabs.
- Stable labels discovered during the quiet window become Pending Renames and may be applied only after revalidating that the same tab is still focused, the candidate is still current, the tab is not manually locked, the visible label still differs, and no newer focus event reset the window.
- `tabby refresh` remains a compatible one-shot action for manual recovery; no refresher IPC is required in the first hybrid slice.
- Manual lock semantics remain unchanged: user labels beat automatic labels until `unlock-focused` or `unlock-all` removes the lock.

## Considered Options

- Keep ADR 0008 one-shot-only refreshes and accept stale labels while the user stays in a tab.
- Roll back to the old daemon model.
- Adopt a Hybrid Session Refresher that restores daemon-like freshness but suppresses expensive/process and UI-changing operations around navigation.

## Consequences

Labels become fresh again while the user remains in the same tab, without requiring manual refresh or a focus event. This reintroduces a long-running per-session process and idempotent startup metadata, so startup complexity returns.

Navigation Stability remains the hard constraint. Tabby still does not rename inactive tabs, and it now also suppresses `pane.process_info` plus `tab.rename` during the 1000 ms quiet window after focus/create events. If this is still not enough, the next mitigation should adjust quiet-window duration or read cadence before considering inactive-tab renames or high-volume events.
