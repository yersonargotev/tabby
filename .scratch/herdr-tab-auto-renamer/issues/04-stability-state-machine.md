# Implement anti-flapping stability

Status: ready-for-agent
Type: task
Blocked by: 03

## Goal

Add Stable Label Candidate state handling before any rename is applied.

## Acceptance criteria

- Poll interval defaults to 500 ms.
- Rename requires two consecutive observations.
- Significant Command to cwd fallback has a 2 second grace period.
- Unit tests cover transient candidate changes.
