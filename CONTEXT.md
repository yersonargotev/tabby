# Herdr Tab Auto-Renamer

This context describes a Herdr plugin that keeps tab labels useful by deriving them from the focused pane's meaningful foreground activity, falling back to the focused pane's working directory name.

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
A Herdr tab whose user-facing label changed outside the plugin's own rename operation, so the plugin must stop auto-renaming it. Manual locks persist across plugin or daemon restarts until an explicit unlock mechanism removes them.
_Avoid_: manual rename, ignored tab, disabled tab

**Unlock Action**:
A user-invoked plugin action that removes one or more Manually Locked Tabs from the persisted lock store so automatic naming can resume. The expected actions are unlock focused tab and unlock all tabs.
_Avoid_: reset, auto-unlock

**Stable Label Candidate**:
A Tab Label Candidate that has survived the plugin's anti-flapping checks and is safe to apply with `tab.rename`. The initial policy is polling every 500 ms, requiring two consecutive observations, and keeping the last Significant Command for a 2 second grace period before falling back to Working Directory Basename.
_Avoid_: immediate label, debounced title

**Focused Pane**:
The pane within a tab that Herdr reports as focused. If no pane in a tab is reported as focused, the plugin may use the first listed pane only for Working Directory Basename fallback unless macOS testing proves Herdr exposes the tab's last-focused pane reliably.
_Avoid_: active pane, selected pane

**Label Policy**:
The rules used to turn process and cwd data into a Tab Label Candidate, including Significant Command allowlists, ignored shells/wrappers, and stability timings. Version 1 uses tested built-in defaults; user configuration is a later slice.
_Avoid_: config, preferences, ruleset

**Process Inspector**:
The boundary that asks Herdr for foreground process details for a selected pane. If process inspection fails or returns no useful Significant Command, the plugin falls back to Working Directory Basename rather than failing the rename loop.
_Avoid_: process_info call, ps lookup
