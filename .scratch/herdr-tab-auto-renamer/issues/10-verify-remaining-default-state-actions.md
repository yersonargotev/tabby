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

## Runtime verification

2026-07-08 manual Herdr verification:

- Baseline before runtime action verification passed: `git status --short` clean at `5b369d5`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt -- --check`.
- Current Herdr state checks:
  - `herdr plugin config-dir yersonargotev.tabby` returned `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby`.
  - `herdr plugin action list --plugin yersonargotev.tabby` listed `start`, `unlock-all`, and `unlock-focused` using `target/debug/tabby`.
  - The real lock store existed at `/Users/argote/.local/state/herdr/plugins/yersonargotev.tabby/locks.json` with an empty v1 store.
  - No fallback lock store existed at `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby/locks.json`.
- After `cargo build` and explicit confirmation, invoked `herdr plugin action invoke unlock-focused --plugin yersonargotev.tabby` without `TABBY_LOCK_STORE_PATH`.
  - Herdr action context focused tab `w2:t1`.
  - The real lock store mtime updated, but contents remained:

    ```json
    {
      "version": 1,
      "locks": {}
    }
    ```

  - Because the store was empty, the semantic outcome was the focused tab was not locked.
  - No fallback lock store was created.
- For `start`, used a contained verification plan after explicit confirmation:
  - Snapshotted current tabs and the real lock store.
  - Temporarily wrote locks for all current tabs (`w1:t1`, `w1:t2`, `w1:t3`, `w2:t1`, `w2:t2`) into the real lock store so the daemon would skip existing tabs.
  - Invoked `herdr plugin action invoke start --plugin yersonargotev.tabby` without `TABBY_LOCK_STORE_PATH`.
  - Herdr returned `plugin-log-8` with `status: running`.
  - The daemon process was PID `15260` (`target/debug/tabby start`) and was stopped with `SIGTERM`.
  - Restored the original empty v1 lock store exactly; final SHA-256 matched the snapshot.
  - Final tab labels matched the pre-run labels (`w1:t1`=`1`, `w1:t2`=`2`, `w1:t3`=`3`, `w2:t1`=`1`, `w2:t2`=`2`).
  - No fallback lock store was created.
- Gotcha: `ps -axo pid,comm,args` showed the local debug binary `comm` truncated as `target/debug/tab`; daemon detection should match `args` containing `target/debug/tabby start`, not `comm == tabby`.
