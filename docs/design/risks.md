# Risks

## Foreground process classification can be wrong

Package runners and wrappers may appear as `node`, shell, or another transient process instead of the user's intended command. Mitigation: conservative Significant Command policy, unit tests, and real macOS process-info fixtures.

## Tab labels may flap

Fast foreground process changes can cause noisy labels. Mitigation: each bounded evaluation requires two consecutive equal candidates, retains a short Significant Command grace period, and prefers Navigation Stability over immediate label freshness.

## Manual lock persistence can surprise users

Persistent locks protect intentional names, but can make tabs look permanently unmanaged. Mitigation: explicit `unlock-focused` and `unlock-all` actions, logs, and documentation.

## Focused pane data may be ambiguous

Inactive tabs may not expose a reliable focused pane. Mitigation: only inspect and rename the currently focused tab; inactive tabs keep their last label until focused again instead of being rewritten from ambiguous pane data.

## Auto-renames can interfere with tab navigation

`tab.rename` mutates Herdr's tab bar. If Tabby performs API work while the user is clicking between tabs, the tab bar can shift or re-render during navigation. Mitigation: each actionable focus trigger resets a 1000 ms Focus Quiet Window with no Herdr API work; afterward one bounded evaluation revalidates current focus and applies at most one rename. Periodic work inspects only the focused tab.

## Plugin trust and installation risk

Herdr plugins run as normal unsandboxed user code. Mitigation: Homebrew release artifacts include checksums, registration remains an explicit `tabby install`, the release manifest resolves its packaged binary, and there is no silent auto-update. Local linking remains available for development.

## API drift or undocumented behavior

Herdr APIs may change or expose platform-specific fields differently. Mitigation: keep Herdr client isolated, include manual compatibility checks, and treat official docs as the source of truth.

## A crashed runtime waits for supported ingress

Herdr 0.8 runs `[[startup]]` for new and restored sessions, but it is not an external process supervisor. If a Ready Session Runtime crashes while the Herdr server continues, recovery waits for the next lifecycle, focus, creation, or manual hook. Mitigation: every supported ingress crosses the Startup Gate and restores exactly one owner; Runtime Status makes an absent or faulted owner diagnosable.
