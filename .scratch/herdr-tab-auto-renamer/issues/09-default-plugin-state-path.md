# Add default plugin-owned state path

Status: needs-triage
Type: task
Blocked by: 08

## Goal

Make Tabby runtime commands usable from Herdr plugin actions without requiring users to export `TABBY_LOCK_STORE_PATH`, while keeping tests and development workflows explicitly injectable.

## Acceptance criteria

- `TABBY_LOCK_STORE_PATH` remains the highest-priority override.
- Runtime commands choose the researched plugin-owned state path when no override is set.
- `daemon`/`start`, `unlock-focused`, and `unlock-all` all use the same lock store path resolution.
- Missing or unsafe state path resolution fails with a clear error instead of writing to an implicit home/config path.
- Unit tests cover override behavior, default path behavior, and refusal behavior where applicable.
- No user config format, installer, release packaging, or implicit auto-unlock is added.
