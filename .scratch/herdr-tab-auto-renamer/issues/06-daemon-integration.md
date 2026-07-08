# Integrate daemon rename loop

Status: ready-for-agent
Type: task
Blocked by: 02, 03, 04, 05

## Goal

Wire Herdr client, Process Inspector, Label Policy, stability, locks, and `tab.rename` into the daemon loop.

## Acceptance criteria

- Unlocked tabs are renamed only when a Stable Label Candidate differs from the current label.
- Manually Locked Tabs are skipped.
- `pane.process_info` failure falls back to cwd basename.
- Logs explain skipped/deferred renames at debug level.
