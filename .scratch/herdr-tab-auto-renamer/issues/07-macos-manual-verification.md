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
