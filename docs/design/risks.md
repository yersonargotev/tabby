# Risks

## Foreground process classification can be wrong

Package runners and wrappers may appear as `node`, shell, or another transient process instead of the user's intended command. Mitigation: conservative Significant Command policy, unit tests, and real macOS process-info fixtures.

## Tab labels may flap

Fast foreground process changes can cause noisy labels. Mitigation: poll every 500 ms, require two consecutive observations, and use a 2 second grace period before command-to-cwd fallback.

## Manual lock persistence can surprise users

Persistent locks protect intentional names, but can make tabs look permanently unmanaged. Mitigation: explicit `unlock-focused` and `unlock-all` actions, logs, and documentation.

## Focused pane data may be ambiguous

Inactive tabs may not expose a reliable focused pane. Mitigation: only use app-first labels when focused-pane confidence is high; otherwise use cwd fallback and document the limitation.

## Plugin trust and installation risk

Herdr plugins run as normal unsandboxed user code. Mitigation: local linking first, no silent auto-update, later releases with checksums and auditable install scripts.

## API drift or undocumented behavior

Herdr APIs may change or expose platform-specific fields differently. Mitigation: keep Herdr client isolated, include manual compatibility checks, and treat official docs as the source of truth.

## Restored Herdr sessions may not auto-start Tabby immediately

Herdr 0.7.3 has documented plugin lifecycle hooks for creation/focus events, but no documented session-start, server-started, or autostart daemon hook. If Herdr restores a session without emitting the `workspace.created` or `tab.created` hooks Tabby uses, the daemon may not start until explicit user action or later lifecycle activity. Mitigation: document the limitation, support `tabby install --start` for the current session, keep the manual `start` action for recovery, and keep a draft upstream Herdr autostart/session-start request in `docs/design/upstream-herdr-autostart-issue.md`. Filing that upstream issue is recommended but not blocking for Tabby's implementation.
