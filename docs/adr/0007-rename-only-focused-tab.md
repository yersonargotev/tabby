# Rename only the focused tab

Tabby will only inspect and auto-rename the currently focused unlocked Herdr tab. Inactive tabs keep their last visible label until the user focuses them again.

## Context

Real usage showed that keeping Tabby active could interfere with mouse navigation between Herdr tabs. The daemon previously considered inactive tabs on every 500 ms tick, including process inspection and possible `tab.rename` calls for inactive single-pane tabs.

`tab.rename` is a UI-changing Herdr API operation. Renaming inactive tabs while the user is clicking around can shift or re-render the tab bar during navigation.

## Consequences

This prioritizes predictable tab navigation over eager background label freshness. A process change in an inactive tab may not be reflected until that tab is focused again, but Tabby will not rewrite inactive tab labels under the user's cursor.

Manual locks are still respected before the inactive-tab skip. Focused tabs keep the existing stability policy: two consecutive observations, a 500 ms poll interval, and the Significant Command grace period.
