# Start Tabby Session Daemons through idempotent ensure-started

Status: Superseded by ADR 0008 for normal CLI/manifest behavior.

Tabby will not launch a long-running daemon implicitly from plain `tabby install`; install remains plugin registration/relink only. Herdr Session startup is explicit with `tabby install --start`, and all normal startup paths—the Herdr `start` action, manifest lifecycle hooks, and install-time startup—must converge on `tabby ensure-started`, which locks and validates one Tabby Session Daemon per Herdr Session before spawning the lower-level `tabby start` loop.

## Considered Options

- Keep `tabby install` as registration only and require manual `herdr plugin action invoke start` every Herdr Session.
- Make plain `tabby install` auto-start the daemon.
- Point Herdr lifecycle hooks directly at `tabby start`.
- Add an external LaunchAgent/service outside Herdr's plugin model.

## Consequences

This preserves the trust boundary that install/registration should not silently start long-running user processes, while still giving users `tabby install --start` for the current Herdr Session and Herdr lifecycle hooks for new activity. The first hook set is `workspace.created` and `tab.created`, both calling `ensure-started`; `pane.created` and focus hooks are reserved for later evidence-driven mitigation.

`ensure-started` owns duplicate prevention. It derives a per-socket `session_key`, uses plugin-owned state for `daemons/<session_key>.lock` and `daemons/<session_key>.json`, validates existing metadata with PID liveness, process identity, and matching session key, then spawns detached `tabby start` only when needed. Restored Herdr Sessions remain a documented limitation because Herdr 0.7.3 has no documented session-start/autostart hook; a draft upstream request lives in `docs/design/upstream-herdr-autostart-issue.md` but is not blocking Tabby's implementation.
