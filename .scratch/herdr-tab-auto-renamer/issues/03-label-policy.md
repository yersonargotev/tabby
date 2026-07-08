# Implement Label Policy

Status: ready-for-agent
Type: task
Blocked by: 01

## Goal

Implement Significant Command and Working Directory Basename candidate derivation with built-in v1 defaults.

## Acceptance criteria

- Tests cover interactive apps, runner/subcommand pairs, shells/wrappers, transient/unknown processes, and cwd basename fallback.
- No user config format is introduced.
- LabelPolicy exists as an internal seam for future config.
