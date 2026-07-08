# Scaffold Rust Herdr plugin

Status: ready-for-agent
Type: task

## Goal

Create the minimal Rust crate and Herdr plugin manifest without implementing rename logic yet.

## Acceptance criteria

- `cargo test` runs.
- `herdr-plugin.toml` exists with local-link development intent.
- CLI has stub commands for daemon/start, `unlock-focused`, and `unlock-all`.
- No installer or release packaging is added.
