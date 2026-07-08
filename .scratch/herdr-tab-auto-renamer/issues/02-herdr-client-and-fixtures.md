# Build Herdr client and fixtures

Status: ready-for-agent
Type: task
Blocked by: 01

## Goal

Implement a small Herdr JSON-RPC client boundary and DTOs for `tab.list`, `pane.list`, `pane.process_info`, and `tab.rename`.

## Acceptance criteria

- Client is isolated behind a module/interface.
- Unit tests cover JSON serialization/deserialization with fixtures.
- Runtime failures are returned as errors without panics.
