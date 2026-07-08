# Verify remaining default-state plugin actions

Status: ready-for-human
Type: task
Blocked by: 09

## Goal

Manually verify the remaining real Herdr plugin actions without `TABBY_LOCK_STORE_PATH`, after the default plugin-owned state path is confirmed for `unlock-all`.

## Acceptance criteria

- Rebuild `target/debug/tabby` before invoking local-linked actions.
- Before each real action invocation, explain the exact expected mutations and get explicit confirmation.
- Verify `unlock-focused` without `TABBY_LOCK_STORE_PATH` uses the same plugin-owned lock store and reports the focused tab outcome.
- Decide whether and how to safely verify `start` without leaving duplicate long-running daemons or unexpectedly renaming real tabs.
- If `start` is verified, record how it was stopped/contained and what tab labels or lock-store entries changed.
- Do not add installer, release packaging, user config format, or implicit auto-unlock.
- Do not edit real user configuration; Herdr/dots-managed config remains untouched.

## Notes

`unlock-all` runtime verification wrote `/Users/argote/.local/state/herdr/plugins/yersonargotev.tabby/locks.json` after rebuilding the local debug binary. The `herdr plugin config-dir` fallback remained `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby`, but no lock store was created there during the successful runtime action.
