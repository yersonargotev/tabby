# Open Decisions

These are intentionally unresolved after the initial grill.

## 1. Exact persisted lock identity

Need to verify whether Herdr tab IDs are stable enough across Herdr restarts for persisted Manually Locked Tabs. If not, the lock store may need a safer key or stale-ID cleanup policy.

## 2. Focused pane semantics for inactive tabs

Need macOS testing to confirm whether `pane.list` exposes the last-focused pane per tab, or only the globally focused pane. Until verified, app-first labels should be conservative for tabs without an explicit focused pane.

## 3. Exact Significant Command allowlist

Initial examples are `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, `npm test`, `go test`, and `cargo run`. The first implementation should make this easy to expand through internal defaults; user config is deferred.

## 4. Process inspection reliability on macOS

Need to collect real `pane.process_info` examples for shells, editors, package runners, and agent CLIs. The architecture assumes graceful fallback to cwd basename if process inspection is incomplete.

## 5. Release/install design

Local linking is v1. Release packaging remains important and should include macOS binaries first, checksums, auditable install script, and no silent auto-update.

## 6. Linux support timing

macOS is first. Linux should be added only after the macOS behavior and process inspection model are stable, unless Linux support falls out for free from the same APIs.
