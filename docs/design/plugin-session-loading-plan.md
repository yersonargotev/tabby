# Plugin Session Loading Implementation Plan

Status: implemented in `7e78988`; verification notes below.

## Final recommendation

Implement Tabby startup as an explicit, idempotent per Herdr Session flow:

1. Keep plain `tabby install` as relink/registration only.
2. Add `tabby install --start` for explicit current Herdr Session startup.
3. Add `tabby ensure-started` as the only normal startup boundary.
4. Route the Herdr `start` action and manifest lifecycle hooks through `ensure-started`.
5. Keep `tabby start` as the lower-level long-running daemon loop used only after `ensure-started` passes duplicate checks.
6. Add `workspace.created` and `tab.created` hooks first; do not add `pane.created` or focus hooks without real verification evidence.
7. Document restored-session limitations until Herdr exposes a stronger startup lifecycle.

## Implementable tasks

1. Extend CLI parsing for `install --start` and `ensure-started`.
   - `tabby install` remains registration only.
   - `tabby install --start` relinks, then invokes the same runtime path as `ensure-started`.

2. Add startup state path helpers.
   - Resolve plugin-owned state base using `HERDR_PLUGIN_STATE_DIR` when present.
   - Reuse explicit fallback rules already used for lock-store path resolution.
   - Create `daemons/` under that state base.

3. Implement per-socket session identity.
   - Resolve the target Herdr socket from runtime context.
   - Prefer canonical socket path when possible.
   - Use a stable hash of an absolute socket path when canonicalization is not available.
   - Fail clearly if no concrete Herdr Session target can be resolved.

4. Implement `ensure-started` locking and metadata.
   - Lock `daemons/<session_key>.lock`.
   - Read `daemons/<session_key>.json` if present.
   - Validate schema version, matching `session_key`, PID liveness, and process identity that appears to be Tabby.
   - Replace stale metadata and spawn detached `tabby start` only when needed.
   - Write metadata with `schema_version`, `pid`, `session_key`, `socket_path`, `started_at`, `tabby_version`, and optional `binary_path`.

5. Update Herdr manifests.
   - Change action id `start` to call `ensure-started` instead of `start`.
   - Add `[[events]]` for `workspace.created` and `tab.created` calling `ensure-started`.
   - Keep dev manifest on `target/debug/tabby` and release manifest on `../../bin/tabby`.

6. Update manifest sync validation.
   - Teach `scripts/check-herdr-manifests.py` about `[[events]]`.
   - Verify dev/release manifests differ only by binary path and intended description/path differences.

7. Add tests.
   - CLI parsing for `install`, `install --start`, `ensure-started`, and rejected extra args.
   - Session key derivation from socket paths.
   - Metadata stale/live validation.
   - Duplicate prevention under lock.
   - Manifest sync checks for actions and events.

8. Run manual verification against Herdr with sandboxed config where possible.
   - `tabby install` does not start a daemon.
   - `tabby install --start` starts exactly one Tabby Session Daemon for the current Herdr Session.
   - Repeated Herdr `start` action invocations remain idempotent.
   - `workspace.created`/`tab.created` hooks start Tabby in new activity.
   - Restore behavior is observed and documented without overpromising.

## Non-goals for the next implementation slice

- No macOS LaunchAgent or external service.
- No heartbeat/control socket.
- No `pane.created` or focus hooks unless verification proves they are necessary.
- No dependency on filing the upstream Herdr issue.

## Implementation verification notes

2026-07-08 implementation checks:

- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `python3 scripts/check-herdr-manifests.py`
- `cargo build`
- Safe smoke test with a temporary `HERDR_PLUGIN_STATE_DIR` and fake absolute `HERDR_SOCKET_PATH` confirmed `target/debug/tabby ensure-started` writes `daemons/<session_key>.json`, spawns `tabby start`, and the spawned process exits when it cannot connect to the fake socket.
- Live Herdr 0.7.3 verification with sandboxed `HOME`, `HERDR_CONFIG_PATH`, and Herdr Session confirmed:
  - `tabby install` relinks the plugin without starting a Tabby Session Daemon.
  - `tabby install --start` starts exactly one Tabby Session Daemon for the current Herdr Session.
  - Repeated Herdr `start` action invocations remain idempotent.
  - `workspace.created` and `tab.created` hooks start a Tabby Session Daemon after the previous daemon is stopped.
  - Restarting the sandboxed Herdr server with restored state does not start a Tabby Session Daemon without a new creation event, so the documented restored-session limitation holds.
