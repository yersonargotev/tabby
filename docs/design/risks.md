# Risks

## Foreground process classification can be wrong

Package runners and wrappers may appear as `node`, shell, or another transient process instead of the user's intended command. Mitigation: conservative Significant Command policy, unit tests, and real macOS process-info fixtures.

## Tab labels may flap

Fast foreground process changes can cause noisy labels. Mitigation: wait briefly during each One-Shot Refresh before inspecting focus/process state, and prefer Navigation Stability over immediate label freshness.

## Manual lock persistence can surprise users

Persistent locks protect intentional names, but can make tabs look permanently unmanaged. Mitigation: explicit `unlock-focused` and `unlock-all` actions, logs, and documentation.

## Focused pane data may be ambiguous

Inactive tabs may not expose a reliable focused pane. Mitigation: only inspect and rename the currently focused tab; inactive tabs keep their last label until focused again instead of being rewritten from ambiguous pane data.

## Auto-renames can interfere with tab navigation

`tab.rename` mutates Herdr's tab bar. If Tabby performs API work while the user is clicking between tabs, the tab bar can shift or re-render during navigation. Mitigation: remove the continuously polling daemon from normal operation; each trigger runs one short refresh, inspects only the tab focused after a short delay, applies at most one rename, and exits.

## Plugin trust and installation risk

Herdr plugins run as normal unsandboxed user code. Mitigation: local linking first, no silent auto-update, later releases with checksums and auditable install scripts.

## API drift or undocumented behavior

Herdr APIs may change or expose platform-specific fields differently. Mitigation: keep Herdr client isolated, include manual compatibility checks, and treat official docs as the source of truth.

## Restored Herdr Sessions may have stale labels until a trigger fires

Herdr 0.7.3 has documented plugin lifecycle hooks for creation/focus events, but no documented Herdr Session-start or server-started hook. If Herdr restores a session without emitting one of Tabby's accepted Refresh Triggers, labels may remain stale until focus/creation activity or the manual refresh action. Mitigation: document the freshness tradeoff and keep Navigation Stability higher priority than always-current labels.
