# Prefer event-triggered label refresh

Status: Superseded by ADR 0009 for normal automatic behavior.

Tabby will move away from a continuously polling daemon as the primary source of automatic label updates. Navigation Stability is more important than label freshness, so Tabby should update labels from explicit Herdr lifecycle/navigation triggers such as tab focus, tab creation, or workspace focus, with any needed stabilization happening after the trigger instead of through constant 500 ms UI-touching polling.

The automatic refresh trigger set is intentionally narrow: tab focus, workspace focus, tab creation, workspace creation, and an explicit manual Start/Refresh action. Tabby should not use pane output changes, layout updates, or continuous polling as automatic label refresh triggers.

Each trigger should start a One-Shot Refresh: wait briefly for focus/process state to settle, inspect only the tab focused at refresh time, and apply at most one automatic rename before exiting. Tabby should not keep a loop alive after the refresh attempt.

The public command name should describe the new behavior. Plugin actions and events should call `tabby refresh`; the old daemon-oriented `ensure-started` command should be removed completely rather than kept as a compatibility alias.

The CLI contract should also stop implying a long-running process: `tabby install` only links or updates the Herdr plugin, `tabby refresh` performs one One-Shot Refresh, and `install --start` should be removed. Any already-running polling daemon from earlier local installs is a one-time local cleanup concern, not permanent product behavior for `install`.

## Considered Options

- Keep the focused-tab-only polling daemon from ADR 0007.
- Prefer event-triggered or one-shot refreshes and let labels lag until the next trigger.

## Consequences

Labels may update later when a foreground process changes while the user remains in the same tab. In exchange, Tabby avoids recurring automatic API activity during mouse navigation, including recurring reads and occasional focused-tab `tab.rename` calls that can disturb the tab bar.
