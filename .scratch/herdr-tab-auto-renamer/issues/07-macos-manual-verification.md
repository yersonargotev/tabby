# Verify behavior on macOS Herdr

Status: ready-for-human
Type: task
Blocked by: 06

## Goal

Manually verify the plugin against real Herdr on macOS.

## Acceptance criteria

- `herdr plugin link .` works locally.
- Labels work for `nvim`, `lazygit`, `pnpm dev`, `codex`, `claude`, and `go test` where Herdr exposes sufficient process info.
- cwd basename fallback works.
- Manual locks survive restart and unlock actions work.
- Focused pane behavior for inactive tabs is recorded in docs/open-decisions.md or resolved into an ADR if needed.

## Comments

2026-07-08: Completed against real macOS Herdr and committed as `e859282 fix: apply macos herdr verification findings`.

Verified:

- `herdr plugin link .` registered enabled local plugin `yersonargotev.tabby`.
- Significant Command labels worked for `nvim`, `lazygit`, `pnpm dev`, `codex`, `claude`, and `go test`.
- cwd basename fallback worked.
- manual locks persisted in an explicit temporary `TABBY_LOCK_STORE_PATH`, survived daemon restart, and explicit `unlock-focused` / `unlock-all` worked.
- inactive two-pane tabs reported no focused pane; documented in `docs/design/open-decisions.md` and enforced in daemon fallback behavior.

Not verified: Herdr tab ID stability across a Herdr/server restart.
