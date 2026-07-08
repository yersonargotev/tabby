# Persist manual rename locks across restarts

Manual tab renames should create Manually Locked Tabs that survive plugin or daemon restarts. This is more stateful than in-memory session locks, but it protects intentional names after Herdr or the plugin restarts and makes manual user intent stronger than automatic naming.

## Consequences

The plugin needs a persisted lock store and an explicit unlock path so users can return a tab to auto-managed naming without deleting all plugin state by hand.
