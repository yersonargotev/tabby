# Draft upstream Herdr issue: plugin session-start/autostart lifecycle

Status: draft only; not filed upstream.

## Problem

Plugins that need one lightweight long-running process per Herdr session do not have a documented startup lifecycle in Herdr 0.7.3. Herdr persists plugin registration in `plugins.json`, but registration does not restart or autostart long-running actions. Current plugin hooks cover lifecycle events such as `workspace.created` and `tab.created`, but there is no documented `session.started`, `server.started`, restored-session, or daemon/autostart entrypoint.

This pushes plugins toward workaround hooks that run on creation or focus events even when the real intent is: ensure one plugin daemon exists for this Herdr session/socket.

## Requested capability

Please consider adding either:

1. A documented plugin event hook such as `session.started`, `server.started`, or `workspace.restored`; or
2. A declarative plugin daemon/autostart entrypoint such as `[[daemons]]` with explicit lifecycle semantics.

Useful semantics for Tabby:

- Runs once per Herdr session/socket for each enabled plugin.
- Receives the same plugin runtime environment as actions/hooks, especially `HERDR_SOCKET_PATH`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, and plugin state/config directories when available.
- Has documented behavior for restored sessions.
- Clearly states whether Herdr restarts the process after crashes or whether plugins must implement their own idempotent ensure-started behavior.
- Avoids requiring plugins to abuse high-frequency focus or pane hooks for daemon startup.

## Current Tabby workaround

Tabby plans to use `workspace.created` and `tab.created` hooks that call an idempotent `tabby ensure-started` command. This is good enough to improve new-session behavior with Herdr 0.7.3, but it cannot honestly promise immediate startup for a fully restored session if no supported creation hook is emitted.
