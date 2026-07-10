# Persist manual rename locks across restarts

Manual tab renames should create Manually Locked Tabs that survive plugin or daemon restarts. This is more stateful than in-memory session locks, but it protects intentional names after Herdr or the plugin restarts and makes manual user intent stronger than automatic naming.

Herdr can reuse a `tab_id` after tab or workspace churn, so the ID alone is not a durable tab identity. Tabby treats a visible label that exactly matches Herdr's reported tab number as the default label of a fresh tab lifecycle. On the first observation of that default-labeled lifecycle it discards the reused ID's persisted lock and plugin-label baseline, resets matching in-memory refresher state, and resumes automatic naming. The refresher prunes runtime state and lifecycle markers when an ID disappears from Herdr's tab list so later reuse starts cleanly. A numeric label that does not match the reported tab number is preserved as possible manual intent.

## Consequences

The plugin needs a persisted lock store and an explicit unlock path so users can return a tab to auto-managed naming without deleting all plugin state by hand. Unlocking a tab removes both its Manual Lock and its plugin-label baseline; otherwise the next refresh could interpret the still-visible manual label as a new external edit and immediately recreate the lock. `unlock-all` applies this only to tabs that were locked, preserving baselines for tabs that remain auto-managed. The Hybrid Session Refresher reloads externally changed persisted lock state before each refresh outside the Focus Quiet Window so actions invoked from another process affect the running refresher.

Explicitly naming tab number `N` as `N` is indistinguishable from Herdr restoring its default label, so that exact label remains auto-managed rather than manually locked.
