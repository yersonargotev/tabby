# Add default plugin-owned state path

Status: ready-for-human
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


## Implementation notes

2026-07-08 implementation:

- Added shared runtime path resolution in `src/paths.rs`.
- `TABBY_LOCK_STORE_PATH` remains the highest-priority override and must be an absolute path.
- Without an override, Tabby uses `HERDR_PLUGIN_STATE_DIR`, then `HERDR_PLUGIN_CONFIG_DIR`, then `herdr plugin config-dir yersonargotev.tabby`, appending `locks.json`.
- Empty or relative resolved paths fail with a clear runtime error.
- `daemon`/`start`, `unlock-focused`, and `unlock-all` all use the same resolver.
- Unit tests cover override, env defaults, `herdr plugin config-dir` default, and refusal behavior.

## Runtime verification

2026-07-08 manual Herdr verification:

- Baseline before runtime action verification passed: `git status --short` clean, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt -- --check`.
- `herdr plugin config-dir yersonargotev.tabby` returned `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby`.
- `herdr plugin action list --plugin yersonargotev.tabby` listed `start`, `unlock-all`, and `unlock-focused` using `target/debug/tabby`.
- With explicit user confirmation, invoked `herdr plugin action invoke unlock-all --plugin yersonargotev.tabby` without `TABBY_LOCK_STORE_PATH`.
- The first invocation failed before writing state because `target/debug/tabby` was stale and still required `TABBY_LOCK_STORE_PATH`.
- After `cargo build`, invoking the same action without `TABBY_LOCK_STORE_PATH` succeeded (`plugin-log-6`, stdout `tabby unlock-all: cleared persisted manual locks`).
- The action wrote the plugin-owned lock store to `/Users/argote/.local/state/herdr/plugins/yersonargotev.tabby/locks.json` with contents:

  ```json
  {
    "version": 1,
    "locks": {}
  }
  ```

- No `locks.json` was created under `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby`, confirming runtime used a higher-priority Herdr plugin-owned state path instead of the `herdr plugin config-dir` fallback.
- `unlock-focused` and long-running `start` were not invoked in this pass; both can mutate real plugin state, and `start` can rename real tabs.
