# Herdr Tab Auto-Renamer

This context describes a Herdr plugin that keeps tab labels useful by deriving them from the focused tab's meaningful foreground activity, falling back to that tab's focused pane working directory name. Tabby prioritizes mouse tab navigation over label freshness; automatic labels may lag rather than disturbing clicks in the tab bar.

## Language

**Tab Label Candidate**:
A short, user-facing label the plugin may apply to a Herdr tab after inspecting the tab's focused pane. It is either a Significant Command label or a Working Directory Basename.
_Avoid_: title, name, tab title

**Significant Command**:
A foreground app or command that is stable and meaningful enough to represent the tab, such as `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, or `go test`. Shells, opaque wrappers, and transient foreground processes are not Significant Commands.
_Avoid_: foreground process, process name, app

**Working Directory Basename**:
The final path component of the focused pane's current working directory, used only when there is no Significant Command candidate. For example, `/Users/me/dev/dots` becomes `dots`.
_Avoid_: full path, cwd label

**Manually Locked Tab**:
A Herdr tab whose user-facing label changed outside the plugin's own rename operation after Tabby has established a plugin-label baseline, so the plugin must stop auto-renaming it. Manual locks persist across plugin runs until an explicit unlock mechanism removes them.
_Avoid_: manual rename, ignored tab, disabled tab

**Unlock Action**:
A user-invoked plugin action that removes one or more Manually Locked Tabs from the persisted lock store so automatic naming can resume. The expected actions are unlock focused tab and unlock all tabs.
_Avoid_: reset, auto-unlock

**Stable Label Candidate**:
A Tab Label Candidate considered safe to apply with `tab.rename` to the currently focused unlocked tab. In the One-Shot Refresh design, the short stabilization delay happens before inspection and Tabby applies at most one candidate from the focused tab before exiting.
_Avoid_: immediate label, debounced title

**Pending Rename**:
A Stable Label Candidate observed during a Focus Quiet Window that the Hybrid Session Refresher may apply only after revalidating that the same tab is still focused, the candidate is still current, the tab is not Manually Locked, the visible label still differs, and no newer focus event reset the window.
_Avoid_: queued title, delayed rename, cached label

**Inactive Tab**:
A Herdr tab that Herdr does not currently report as focused. The Hybrid Session Refresher does not inspect processes or apply renames to Inactive Tabs; their last visible label is preserved until a later refresh sees them focused and outside the Focus Quiet Window.
_Avoid_: background tab, hidden tab

**Navigation Stability**:
The user-facing guarantee that clicking or otherwise navigating between Herdr tabs must not be disrupted by Tabby's automatic label updates. Navigation Stability is more important than immediate label freshness.
_Avoid_: click workaround, UI quirk, placebo fix

**Focus Quiet Window**:
A short interval after a Herdr tab or workspace focus change during which the Hybrid Session Refresher must not call `tab.rename` or `pane.process_info`. The window resets on each new focus change; the accepted default is 1000 ms.
_Avoid_: debounce, delay, cooldown

**Refresh Trigger**:
A discrete Herdr navigation or lifecycle event, or an explicit user action, that permits Tabby to evaluate whether the focused tab label should change. Accepted Refresh Triggers are tab focus, workspace focus, tab creation, workspace creation, and manual refresh.
_Avoid_: polling signal, output event, every tick

**One-Shot Refresh**:
A bounded automatic label refresh attempt started by a Refresh Trigger. It may wait briefly for the focused tab to settle, inspect the focused tab, and apply at most one automatic label update before ending.
_Avoid_: daemon loop, background polling, continuous refresh

**Focused Pane**:
The pane within the focused tab that Herdr reports as focused. If no pane in the focused tab is reported as focused, the plugin may use the first listed pane only for Working Directory Basename fallback.
_Avoid_: active pane, selected pane

**Label Policy**:
The rules used to turn process and cwd data into a Tab Label Candidate, including Significant Command allowlists, ignored shells/wrappers, and stability timings. Version 1 uses tested built-in defaults; user configuration is a later slice.
_Avoid_: config, preferences, ruleset

**Process Inspector**:
The boundary that asks Herdr for foreground process details for a selected pane. If process inspection fails or returns no useful Significant Command, the plugin falls back to Working Directory Basename rather than failing the rename loop.
_Avoid_: process_info call, ps lookup

**Herdr Session**:
A running Herdr server context identified by the socket that plugin commands use to inspect and rename tabs. Tabby's automatic behavior is scoped to one Herdr Session at a time.
_Avoid_: terminal session, shell session

**Tabby Session Daemon**:
A legacy long-running Tabby process from the superseded pre-hybrid polling design. Use Hybrid Session Refresher for current behavior.
_Avoid_: current refresh process, plugin action process

**Hybrid Session Refresher**:
A long-running Tabby process scoped to one Herdr Session that restores automatic label freshness while preserving Navigation Stability. It may observe the focused tab continuously, but it must not inspect or rename Inactive Tabs, and it must suppress or defer `tab.rename` around tab/workspace focus changes until the focused tab has settled.
_Avoid_: old daemon, polling daemon, background renamer
