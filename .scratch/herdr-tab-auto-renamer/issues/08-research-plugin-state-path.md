# Research plugin-owned state path

Status: ready-for-agent
Type: research
Blocked by: 07

## Goal

Determine the safest plugin-owned state path for Tabby's persisted lock store when launched by Herdr plugin actions, without introducing user-editable config or release/install packaging.

## Acceptance criteria

- Identify whether Herdr exposes a plugin config/state directory to plugin actions, either through environment variables, action cwd, or `herdr plugin config-dir yersonargotev.tabby`.
- Confirm whether plugin actions receive `HERDR_SOCKET_PATH` automatically when invoked by Herdr.
- Document the recommended state path decision in `docs/design/open-decisions.md` or a new ADR if resolved.
- Keep `TABBY_LOCK_STORE_PATH` as an explicit override for tests/dev.
- Do not edit real user configuration directly; config is managed by dots.
- Before invoking real Herdr plugin actions, explain expected mutations and ask for confirmation.

## Notes

Slice 07 verified the daemon with an explicit temporary `TABBY_LOCK_STORE_PATH`. Runtime still intentionally refuses to infer a real state path.
