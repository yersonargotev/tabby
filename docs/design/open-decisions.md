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
