# Implement persistent manual locks and unlock actions

Status: ready-for-agent
Type: task
Blocked by: 01

## Goal

Persist Manually Locked Tabs and expose explicit unlock actions.

## Acceptance criteria

- Manual rename detection is tested.
- Locks survive daemon restart via a persisted lock store.
- `unlock-focused` removes one lock.
- `unlock-all` clears all locks.
- Stale lock behavior is documented or safely handled.
