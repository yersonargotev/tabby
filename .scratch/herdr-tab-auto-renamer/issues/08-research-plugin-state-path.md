# Research plugin-owned state path

Status: ready-for-human
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


## Findings

2026-07-08 research results:

- Baseline passed before research: `cargo test` and `cargo clippy --all-targets -- -D warnings`.
- `herdr plugin config-dir yersonargotev.tabby` returned `/Users/argote/.config/herdr/plugins/config/yersonargotev.tabby`; the directory exists and is empty before Tabby writes plugin state.
- `herdr plugin action list --plugin yersonargotev.tabby` lists `start`, `unlock-all`, and `unlock-focused` using relative commands under the local-linked plugin root.
- Invoking actions with `TABBY_LOCK_STORE_PATH` set in the caller environment did not pass that variable into the action process; the current safe runtime refusal prevented writes.
- A temporary diagnostic wrapper for `target/debug/tabby` showed action cwd is the plugin root, argv is passed as configured, and the action env contained no `HERDR_*` or `TABBY_*` variables in CLI invocation.
- The same diagnostic wrapper showed `herdr` is on `PATH` inside the action process and `herdr plugin config-dir yersonargotev.tabby` works from there.

Recommendation documented in `docs/design/open-decisions.md`: keep `TABBY_LOCK_STORE_PATH` as override; default to `locks.json` under Herdr plugin-owned state/config directories, falling back to the `herdr plugin config-dir` command; reject empty or relative resolved paths.
