# PRD: Herdr Tab Auto-Renamer

Status: needs-triage

## Problem

Herdr tabs can end up with numeric or stale manual labels. Users need tab labels that reflect the current meaningful activity without showing full paths or noisy transient processes.

## Desired behavior

- For each tab, inspect the Focused Pane.
- Prefer a Significant Command label such as `nvim`, `pnpm dev`, `lazygit`, `codex`, `claude`, or `go test`.
- If no Significant Command is present, use the Working Directory Basename.
- Never use full paths as labels.
- Preserve Manually Locked Tabs across restarts.
- Avoid flapping with stability checks.
- Support macOS first; Linux later if low-friction.

## Non-goals for v1

- User-editable config file.
- Remote install script / release packaging.
- Linux-first validation.
- Forking `lmilojevicc/herdr-tab-rename`.

## Key docs

- `CONTEXT.md`
- `docs/design/architecture.md`
- `docs/design/open-decisions.md`
- `docs/design/risks.md`
- `docs/adr/0001-persist-manual-rename-locks.md`
- `docs/adr/0002-build-from-scratch.md`
- `docs/adr/0003-use-rust.md`
- `docs/adr/0004-local-link-first-distribution.md`
