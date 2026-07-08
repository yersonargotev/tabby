# Plugin Session Loading Research

Date: 2026-07-08
Status: research input; design accepted in `docs/adr/0006-start-daemons-through-ensure-started.md` and implementation plan in `docs/design/plugin-session-loading-plan.md`.

## Goal

Find a way for Tabby to load in both the current Herdr session and future Herdr sessions so users do not have to run this manually in every session:

```sh
herdr plugin action invoke start --plugin yersonargotev.tabby
```

## Current Tabby behavior

- Tabby's Herdr manifests only declare actions. The `start` action runs `tabby start`; there are no `[[events]]` hooks today. Sources: `herdr-plugin.toml`, `packaging/herdr/herdr-plugin.toml`.
- `tabby start` enters `daemon::run_daemon_loop_from_env()`, connects to the Herdr socket from the runtime environment, and loops forever polling/renaming tabs. Sources: `src/lib.rs`, `src/daemon.rs`.
- `tabby install` currently relinks the Homebrew-managed plugin root, then tells the user how to start it manually. Source: `src/install.rs`.

## Primary-source findings

### Herdr persists plugin registration, not a running action

Herdr's plugin registry persists installed/linked plugins across restarts. The socket API docs say `plugin.link`, `plugin.unlink`, `plugin.enable`, and `plugin.disable` write `plugins.json` next to `session.json`; startup reloads that registry and re-reads manifests from their original paths. Source: https://herdr.dev/docs/socket-api/#plugin-apis

That persistence makes Tabby remain *registered* for future Herdr starts, but it does not imply that Herdr restarts an action that was previously invoked.

### Actions are explicit process launches

The Herdr CLI docs define `herdr plugin action invoke <action_id> [--plugin ID]` as the command that starts a manifest action. The socket API docs likewise say `plugin.action.invoke` resolves the manifest action, starts the manifest command, and returns command-log metadata. Sources: https://herdr.dev/docs/cli-reference/#plugins and https://herdr.dev/docs/socket-api/#plugin-apis

This matches Tabby's current UX problem: `start` is a long-running action, so the user has to launch that process in each Herdr server/session unless Tabby adds another trigger.

### Plugin commands receive session-specific runtime context

Herdr runtime commands run from the plugin directory and receive `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_PLUGIN_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_CONFIG_DIR`, `HERDR_PLUGIN_STATE_DIR`, and context ids when available. Source: https://herdr.dev/docs/plugins/#commands-and-environment

Because `tabby start` connects to the socket exposed for that invocation, one daemon process is naturally scoped to the Herdr session/socket that launched it. New Herdr named sessions use distinct sockets; the socket docs list default and named-session socket paths and the resolution order (`--session`, `HERDR_SOCKET_PATH`, `HERDR_SESSION`, default). Source: https://herdr.dev/docs/socket-api/#socket-paths

### Herdr plugin v1 has manifest event hooks

Herdr plugin manifests can include `[[events]]` hooks. The plugin docs show event hooks in the manifest, and the socket API says event hooks run for enabled installed plugins when Herdr emits a matching event name. Sources: https://herdr.dev/docs/plugins/#manifest and https://herdr.dev/docs/socket-api/#plugin-apis

Herdr validates hook `on` values against known event names at link time. Unknown names do not fail the link, but warnings are surfaced in `plugin.link`/`plugin.list`. Source: https://herdr.dev/docs/socket-api/#plugin-apis

The current Herdr 0.7.3 CLI schema exposes lifecycle event names such as `workspace.created`, `workspace.focused`, `tab.created`, `tab.focused`, `pane.created`, `pane.focused`, and more. Verified locally with:

```sh
XDG_CONFIG_HOME="$(mktemp -d)/config" herdr api schema --json
```

The Herdr source also defines `PLUGIN_HOOK_EVENT_KINDS` with workspace, worktree, tab, pane, and agent lifecycle events, and explicitly excludes high-volume pane output-change hooks from plugin hooks. Source: https://raw.githubusercontent.com/ogulcancelik/herdr/master/src/api/schema/events.rs

### No documented plugin startup/autostart hook exists today

The official plugin docs describe manifest-declared actions, event hooks, panes, and link handlers. The CLI/socket docs list plugin methods for link/list/enable/disable/action/pane/log operations. None of the official docs found a `startup`, `server.started`, `session.started`, `autostart`, or persistent plugin-daemon manifest field. Sources: https://herdr.dev/docs/plugins/, https://herdr.dev/docs/cli-reference/#plugins, https://herdr.dev/docs/socket-api/#raw-methods

## Options

### Option A — `tabby install --start` or implicit start for the current session

After relinking, `tabby install` can also invoke the `start` action for the currently reachable Herdr session, equivalent to:

```sh
herdr plugin action invoke start --plugin yersonargotev.tabby
```

Pros:

- Solves the current-session pain immediately after install or upgrade.
- Uses Herdr's documented action invocation path.
- Keeps trust explicit: the user opted into `tabby install`.

Cons:

- Does not solve future sessions by itself.
- Needs idempotency so repeated installs do not spawn duplicate `tabby start` loops for the same Herdr socket.

Implementation notes:

- Add an `ensure-started`/idempotency layer before launching or inside `tabby start`.
- Use the same stale `HERDR_SOCKET_PATH` handling already added for Herdr subprocesses.
- Consider making auto-start opt-in (`tabby install --start`) if silent process launch during install feels too surprising.

### Option B — Add manifest event hooks that run `tabby ensure-started`

Add `[[events]]` entries to both manifests for a small command that ensures exactly one Tabby daemon is running for the invoking Herdr socket/session. Candidate hooks:

```toml
[[events]]
on = "workspace.created"
command = ["../../bin/tabby", "ensure-started"]

[[events]]
on = "tab.created"
command = ["../../bin/tabby", "ensure-started"]

[[events]]
on = "pane.created"
command = ["../../bin/tabby", "ensure-started"]
```

Use `target/debug/tabby` in the development manifest and `../../bin/tabby` in the release manifest, matching the existing action path convention.

Pros:

- Uses Herdr's first-party plugin mechanism.
- Helps future sessions without the user manually invoking the `start` action.
- Hooks receive the right session-specific socket environment, so Tabby can start a daemon for the session that emitted the event.

Cons:

- Herdr has no documented startup hook. Event hooks only fire when their lifecycle event occurs; a restored session with existing tabs may not trigger until something is created/focused, depending on Herdr's restore behavior.
- Needs strong duplicate protection because multiple lifecycle events can fire close together.
- Hooks should be very cheap; they must not run the full daemon inline repeatedly.

Implementation notes:

- Prefer `ensure-started` over pointing events directly at `start`. `ensure-started` should acquire a per-session lock and then spawn/detach `tabby start` only if absent.
- Key the per-session lock by canonical socket path or a stable hash of `HERDR_SOCKET_PATH`/`HERDR_SESSION` so named sessions do not conflict.
- Store PID/metadata in `HERDR_PLUGIN_STATE_DIR`, not the plugin root. Herdr docs reserve plugin root for managed source and state/config dirs for plugin-owned durable files.
- Keep `start` as a manual action for recovery/debugging.
- After linking, check `herdr plugin list --plugin yersonargotev.tabby --json` and fail/warn if Herdr reports unknown event warnings.

### Option C — A single external launcher/LaunchAgent watches Herdr sockets

Ship a separate macOS user LaunchAgent or `tabby service` that watches Herdr socket locations and starts a Tabby daemon per live session.

Pros:

- Can cover sessions even if no Herdr plugin lifecycle event fires.
- Can potentially restart daemons after crashes.

Cons:

- Mutates real user OS configuration and goes beyond Herdr's plugin v1 model.
- More complicated install/uninstall, logs, permissions, and trust surface.
- Needs robust discovery for named-session sockets and stale sockets.

This is probably too heavy for the next Tabby iteration unless event hooks prove insufficient.

### Option D — Ask Herdr upstream for plugin autostart/session-start support

Request a Herdr manifest feature such as `[[events]] on = "server.started"` / `session.started`, or a dedicated `[[daemons]]`/`autostart = true` plugin entrypoint.

Pros:

- Best long-term host-level semantics.
- Avoids lifecycle-event workarounds.

Cons:

- Requires upstream change; not available in Herdr 0.7.3 based on docs and local schema.

## Recommendation

Use a two-part approach:

1. **Current sessions:** extend `tabby install` with an explicit `--start` path, or make install print and optionally run a new `tabby ensure-started` command. This gives immediate current-session coverage after install/upgrade.
2. **New sessions:** add Herdr manifest event hooks (`workspace.created`, `tab.created`, and/or `pane.created`) that call `tabby ensure-started`, not `tabby start` directly. `ensure-started` must be idempotent per Herdr socket/session and should spawn/detach the real long-running daemon only when needed.

This stays inside Herdr's documented plugin v1 surfaces and avoids requiring users to manually invoke the `start` action for every session. The main remaining gap is restored sessions with no emitted lifecycle event; track that as either a documented limitation, an extra `workspace.focused`/`pane.focused` hook if not too noisy, or an upstream Herdr feature request for a real session-start/autostart hook.

## Verification commands used

```sh
herdr --version
herdr plugin --help
XDG_CONFIG_HOME="$(mktemp -d)/config" herdr api schema --json
```
