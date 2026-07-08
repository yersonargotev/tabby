# Open Decisions

These are intentionally unresolved after the initial grill.

## 1. Exact persisted lock identity

Need to verify whether Herdr tab IDs are stable enough across Herdr restarts for persisted Manually Locked Tabs. If not, the lock store may need a safer key or stale-ID cleanup policy.

## 2. Focused pane semantics for inactive tabs

Need macOS testing to confirm whether `pane.list` exposes the last-focused pane per tab, or only the globally focused pane. Until verified, app-first labels should be conservative for tabs without an explicit focused pane.

2026-07-08 macOS Herdr verification: created a two-pane test tab, focused its second pane, then focused another tab. `pane.list` reported `focused: false` for both panes in the inactive test tab. V1 should therefore treat `pane.focused=true` as global focus evidence only; when a tab has no explicitly focused pane, use the first listed pane only for Working Directory Basename fallback and do not call `pane.process_info` for app-first labels.

## 3. Exact Significant Command allowlist

Initial examples are `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, `npm test`, `go test`, and `cargo run`. The first implementation should make this easy to expand through internal defaults; user config is deferred.

## 4. Process inspection reliability on macOS

Need to collect real `pane.process_info` examples for shells, editors, package runners, and agent CLIs. The architecture assumes graceful fallback to cwd basename if process inspection is incomplete.

2026-07-08 macOS Herdr verification:

- `nvim`, `lazygit`, `go test`, `codex`, and `claude` exposed enough foreground process info for Significant Command labels.
- `pnpm dev` installed through the local pnpm toolchain appeared as `node .../pnpm.mjs dev` plus the child Node process, so the label policy recognizes the pnpm Node shim shape in addition to direct `pnpm dev`.

## 5. Release/install design

Local linking is v1. Release packaging remains important and should include macOS binaries first, checksums, auditable install script, and no silent auto-update.

## 6. Linux support timing

macOS is first. Linux should be added only after the macOS behavior and process inspection model are stable, unless Linux support falls out for free from the same APIs.

## 7. Plugin-owned lock store path

2026-07-08 Herdr plugin runtime research resolved the v1 default:

- `herdr plugin config-dir yersonargotev.tabby` returns `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby` for the local-linked plugin, and the directory exists as plugin-owned Herdr state/config space.
- `herdr plugin action invoke` runs action commands from the plugin root, so the current relative `target/debug/tabby` commands resolve even when the invoking shell cwd is elsewhere.
- CLI-invoked plugin actions did not inherit arbitrary caller env (`TABBY_LOCK_STORE_PATH`) and did not expose `HERDR_SOCKET_PATH`, `HERDR_PLUGIN_CONFIG_DIR`, or `HERDR_PLUGIN_STATE_DIR` in the action process env. The Herdr CLI is available inside the action process, and `herdr plugin config-dir yersonargotev.tabby` works there.

Decision: keep `TABBY_LOCK_STORE_PATH` as the highest-priority explicit override for tests/development. Without it, resolve `locks.json` inside Herdr-provided plugin-owned state/config directories if Herdr exposes them, otherwise call `herdr plugin config-dir yersonargotev.tabby` and use `<config-dir>/locks.json`. Reject empty or relative resolved paths rather than writing to an invented implicit home/config path.

2026-07-08 runtime verification:

- A real `herdr plugin action invoke unlock-all --plugin yersonargotev.tabby` run failed before rebuilding because the local-linked action command points at `target/debug/tabby`, and that binary was stale from before the default state-path change.
- After `cargo build`, the same action succeeded without `TABBY_LOCK_STORE_PATH`.
- The observed lock store path was `/Users/argote/.local/state/herdr/plugins/yersonargotev.tabby/locks.json`, with an empty v1 store. No `locks.json` was created under the `herdr plugin config-dir` fallback path.
- `unlock-focused` also used the same runtime state path without `TABBY_LOCK_STORE_PATH`; with focused tab `w2:t1` and an empty store, it left the empty v1 store in place and did not create fallback state.
- `start` was verified by temporarily locking all current tabs in the real lock store, invoking the action, stopping the spawned `target/debug/tabby start` process with `SIGTERM`, and restoring the original empty v1 store. No tab labels changed and no fallback state was created.
- This confirms the implemented precedence can use Herdr plugin-owned state space at runtime; local-link verification must rebuild `target/debug/tabby` after source changes before invoking plugin actions.

## 8. Plugin startup for current and future Herdr Sessions

2026-07-08 grilling resolved the install/start boundary:

- Plain `tabby install` remains relink/registration only and must not launch a long-running daemon implicitly.
- Herdr Session startup is explicit via `tabby install --start`.
- `tabby install --start` must call the same idempotent startup path as lifecycle hooks: `tabby ensure-started`.
- Neither install-time startup nor manifest event hooks should call `tabby start` directly, because `start` is the long-running daemon loop and does not protect against duplicate Tabby Session Daemons by itself.

Still unresolved in this design session:

- None for the Herdr Session-loading design; remaining work should move into ADR and implementation tasks.

2026-07-08 grilling resolved the `ensure-started` contract:

- `tabby ensure-started` is the startup concurrency boundary, not a thin PID check.
- It resolves the target Herdr socket for the invoking runtime and derives a stable per-socket `session_key`.
- Prefer the canonical socket path when possible; if canonicalization fails but the socket path is absolute, use a stable hash of the textual path.
- Do not key duplicate detection only by `HERDR_SESSION`, because it may be absent and does not cover explicit socket overrides.
- Acquire a lock file scoped to that `session_key` before inspecting or writing daemon metadata.
- Under the lock, validate any existing daemon metadata by both liveness and matching `session_key`/socket identity.
- If the matching daemon is alive, exit successfully without spawning.
- If metadata is stale or missing, spawn a detached `tabby start` that inherits enough environment to connect to the same Herdr socket, then write fresh metadata.

2026-07-08 grilling resolved the daemon metadata layout:

- Keep durable user/product state and ephemeral daemon state separate.
- `locks.json` remains the durable Manually Locked Tab store.
- Store startup metadata under the plugin-owned state base as `daemons/<session_key>.json`.
- Store the concurrency lock for that daemon as `daemons/<session_key>.lock`.
- Prefer `HERDR_PLUGIN_STATE_DIR` for this state. If Herdr does not expose it, use the same explicit plugin-owned state/config resolution rules as the existing lock-store path code, but do not write to the plugin root or an invented implicit home path.
- Treat `daemons/*.json` as operational cache: it may be deleted without losing user preferences, and stale entries should be replaced when the recorded process is dead or no longer matches the same `session_key`.
- Include at least `schema_version`, `pid`, `session_key`, `socket_path`, `started_at`, `tabby_version`, and optionally `binary_path` for debugging.

2026-07-08 grilling resolved the initial manifest hook set:

- Add Herdr lifecycle hooks for `workspace.created` and `tab.created`.
- Both hooks should call `tabby ensure-started`, using `target/debug/tabby` in the development manifest and `../../bin/tabby` in the release manifest.
- Do not point hooks at `tabby start` directly.
- Keep `pane.created` out of v1 unless real Herdr verification shows `workspace.created` + `tab.created` do not start Tabby early enough.
- Keep focus hooks such as `workspace.focused`, `tab.focused`, and `pane.focused` out of v1 unless restore-session behavior is unacceptable without them.
- Keep the `start` action as a manual recovery/debug path.

2026-07-08 grilling resolved the v1 restore-session promise:

- Do not claim Tabby always starts immediately when Herdr opens or restores a session.
- Promise automatic startup only when Herdr emits the supported creation hooks (`workspace.created` and `tab.created`) for an enabled installed plugin.
- A fully restored Herdr Session that does not emit those creation hooks may require explicit startup until Herdr exposes a stronger session-start/autostart plugin lifecycle.
- Supported workaround: run `tabby install --start` after install/upgrade, or use the manual plugin action for startup/recovery.
- Adding `pane.created` or focus hooks is a future mitigation only if real verification shows the limitation is unacceptable.

2026-07-08 grilling resolved upstream issue handling:

- Do not block Tabby implementation on creating an upstream Herdr issue.
- Document a draft upstream request for a session-start/autostart plugin lifecycle so it is easy to publish later.
- The Tabby feature is considered implementable with available Herdr 0.7.3 resources using `ensure-started` plus `workspace.created`/`tab.created` hooks.
- Development should only block if real verification shows Herdr does not reliably emit the chosen creation hooks for new Herdr Sessions.

2026-07-08 grilling resolved the user-facing Herdr startup action:

- Keep the Herdr action id `start` for compatibility and simple UX.
- Change the `start` action command to run `tabby ensure-started`, not `tabby start`.
- Do not add a second visible Herdr action named `ensure-started`; that would duplicate startup concepts for users.
- Keep `tabby start` as the lower-level CLI command that runs the long-lived daemon loop, used by `ensure-started` after duplicate checks pass.
- Normal startup paths should all converge on `ensure-started`: `tabby install --start`, manifest event hooks, and the Herdr `start` action.

2026-07-08 grilling resolved v1 daemon liveness validation:

- Do not treat PID existence alone as proof that a Tabby daemon is running, because PIDs can be recycled.
- Validate daemon metadata with all of:
  - the recorded process is alive;
  - the observed process command/name/path appears to be Tabby;
  - the metadata `session_key` matches the target Herdr Session.
- If any check fails, treat the metadata as stale and allow `ensure-started` to replace it while holding the per-session lock.
- Do not add a heartbeat or daemon control socket in v1; that would add a larger IPC surface than this iteration needs.
- Document this as best-effort liveness detection.
