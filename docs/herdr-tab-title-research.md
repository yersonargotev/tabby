# Herdr dynamic tab title research

Date: 2026-07-07

## Question

Can Herdr tabs automatically show the app/command currently running in the tab, and otherwise show only the basename of the current working directory? If yes, should dots solve it with Herdr configuration, by installing/configuring an existing plugin, or by creating a new Herdr plugin?

## Executive summary

Herdr configuration alone is not enough for fully dynamic tab titles. The current config surface covers terminal startup behavior, keybindings, UI options, and whether new tabs prompt for a name, but it does not expose a declarative rule such as "tab title = foreground process or cwd basename". Herdr does expose the needed runtime data and controls through its socket/CLI API: tabs can be renamed, panes expose process/cwd information, and `pane.get`/`pane.list` expose `foreground_cwd` when available.

There is already a community plugin, [`lmilojevicc/herdr-tab-rename`](https://github.com/lmilojevicc/herdr-tab-rename), that auto-renames each tab to the basename of the focused pane's working directory and preserves manual renames. That covers the fallback path (`pwd` basename), but not the preferred "running app first" behavior.

Recommendation: do not change dots' Herdr config for this as the primary solution. First test the existing cwd-only plugin. If the app-first behavior is required, build or fork a small Herdr plugin/daemon that uses `pane.process_info` or `pane.list` plus `tab.rename` to compute labels such as `pnpm`, `nvim`, `lazygit`, or `dots`.

## Evidence from primary sources

### Herdr configuration can set static behavior, not a dynamic title rule

The official configuration docs show Herdr's terminal cwd policy under `[terminal]`: `new_cwd = "follow"` inherits from the source pane/workspace, while `home`, `current`, or a fixed path are alternatives. This affects where new panes/tabs start; it is not a tab-title template or runtime renaming rule. Source: <https://herdr.dev/docs/configuration/>.

The same docs list UI options including `prompt_new_tab_name`, which controls whether Herdr asks for a tab label when creating a tab. They also list tab keybindings such as `new_tab`, `rename_tab`, `previous_tab`, `next_tab`, `switch_tab`, and `close_tab`. These are manual/static controls, not dynamic naming logic. Source: <https://herdr.dev/docs/configuration/>.

The locally installed Herdr 0.7.2 default config confirms the exposed config keys: `prompt_new_tab_name`, tab keybindings, and `show_agent_labels_on_pane_borders`, but no `tab_title_format`, `auto_rename_tabs`, or equivalent dynamic tab-title option was present (`herdr --default-config`, run 2026-07-07).

### Herdr has the API needed for a plugin/daemon

Herdr's official Socket API docs list `tab.rename`, `tab.list`, `pane.list`, and `pane.process_info` among controllable methods. `pane.process_info` returns shell pid, foreground process group id, and foreground processes with pid, name, argv/cmdline, and cwd where the platform exposes them. Source: <https://herdr.dev/docs/socket-api/>.

The Socket API docs also say `pane.get`, `pane.list`, `agent.get`, and `agent.list` expose `foreground_cwd` when Herdr can resolve the cwd of the foreground process; the existing `cwd` remains the pane/workspace cwd for labels, follow-cwd behavior, and restore. Source: <https://herdr.dev/docs/socket-api/>.

The same docs document `pane.report_metadata`, which can override a pane title and related presentation fields, but the user's desired surface is the tab label, so `tab.rename` is the direct API for this use case. Source: <https://herdr.dev/docs/socket-api/>.

The official Concepts docs define a tab as a layout inside a workspace and state that tabs are addressable from the CLI and socket API. Panes are real terminals and can be renamed manually. Source: <https://herdr.dev/docs/concepts/>.

### Herdr plugins are executable workflow packages with full CLI/socket access

The official Plugins docs say a plugin is a directory with a `herdr-plugin.toml` manifest and commands Herdr can launch; plugin commands call back into Herdr through the CLI or socket API. Herdr does not provide a separate restricted SDK: the Herdr CLI/socket API is the plugin surface. Source: <https://herdr.dev/docs/plugins/>.

Plugin manifests can declare actions and event hooks; runtime commands receive Herdr context via environment variables including `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_ENV=1`, and available workspace/tab/pane ids. Source: <https://herdr.dev/docs/plugins/>.

Plugin docs also warn that plugins are ordinary code running as the user and are not sandboxed or reviewed by Herdr, so any third-party plugin should be vetted or pinned before dots installs it automatically. Source: <https://herdr.dev/docs/plugins/>.

### Existing plugin: cwd basename auto-renaming already exists

[`lmilojevicc/herdr-tab-rename`](https://github.com/lmilojevicc/herdr-tab-rename) describes itself as a Herdr plugin that automatically renames each tab to the basename of its focused pane's current working directory and leaves manually renamed tabs alone. Its README lists Herdr >= 0.7.0, macOS/Linux support, and installation with `herdr plugin install lmilojevicc/herdr-tab-rename`. Source: <https://github.com/lmilojevicc/herdr-tab-rename>.

The plugin's README says it polls every 500ms over the Herdr Unix socket, reads `tab.list` and `pane.list`, and calls `tab.rename` as needed. Its desired label is the basename of the focused pane's `foreground_cwd`, falling back to `cwd`. Source: <https://github.com/lmilojevicc/herdr-tab-rename>.

The plugin source backs that up: `daemon.go` defines tab and pane structs with `Label`, `CWD`, and `ForegroundCWD`, builds a desired label from `filepath.Base(cwd)`, and calls JSON-RPC method `tab.rename`. Source: <https://raw.githubusercontent.com/lmilojevicc/herdr-tab-rename/main/daemon.go>.

This existing plugin does **not** claim to inspect the foreground process name or argv; its stated behavior is cwd-only. Therefore it covers the fallback half of the user's request, not the full app-first policy. Source: <https://github.com/lmilojevicc/herdr-tab-rename>.

### Adjacent plugins do not solve this exact request

[`rjyo/herdr-window-title-sync`](https://github.com/rjyo/herdr-window-title-sync) syncs the **outer terminal window/tab title** to the focused Herdr workspace/tab/agent session. It is useful for host terminal titles, but it does not rename Herdr's internal tabs according to process/cwd. Source: <https://github.com/rjyo/herdr-window-title-sync>.

[`wyattjoh/herdr-plugin-renamer`](https://github.com/wyattjoh/herdr-plugin-renamer) renames numeric tabs from a coding agent's first prompt and can also rename a linked worktree branch/workspace. This targets agent task naming, not process/cwd-based tab labels. Source: <https://github.com/wyattjoh/herdr-plugin-renamer>.

Herdr's official plugin marketplace states listings are community plugins discovered via the `herdr-plugin` GitHub topic and are not reviewed by Herdr. Source: <https://herdr.dev/plugins/>.

## Options

### Option A — dots-only Herdr config

**Can solve full need?** No.

Dots can manage `~/.config/herdr/config.toml`, and Herdr config can keep useful supporting defaults such as `new_cwd = "follow"` and `prompt_new_tab_name = true/false`. But no official config key found in the docs or default config expresses dynamic labels based on foreground process or cwd basename.

**Use dots for:** keeping Herdr config sane; optionally adding a keybinding that manually invokes a plugin action after the plugin exists.

### Option B — install an existing plugin

**Can solve full need?** Partially.

`lmilojevicc/herdr-tab-rename` already solves "if nothing special is running, show folder basename" by polling `foreground_cwd`/`cwd` and calling `tab.rename`. It is a good low-effort first experiment if cwd-only is acceptable.

**Gap:** it does not prioritize running app/command names like `pnpm`, `nvim`, or `lazygit`.

**dots fit:** dots could eventually install/link this as a managed dependency or document a manual install, but because Herdr plugins execute arbitrary user-level code and are not sandboxed, the safer first step is manual install or vendored/reviewed plugin code rather than silently adding third-party plugin installation to a core profile.

### Option C — build or fork a plugin for app-first labels

**Can solve full need?** Yes.

A small daemon/plugin can:

1. enumerate tabs with `tab.list`;
2. enumerate panes with `pane.list` or inspect focused panes with `pane.process_info`;
3. for each tab, pick the focused pane;
4. if a foreground process is meaningful and not just the shell, derive a short label from the process name/argv (`nvim`, `pnpm dev`, `lazygit`, etc.);
5. otherwise use `basename(foreground_cwd || cwd)`;
6. call `tab.rename` only when the label changes;
7. preserve manual renames using the same lock/baseline idea as `herdr-tab-rename`.

**dots fit:** dots could own this as a reviewed local plugin after a design pass, or keep it in a separate plugin repo and add dots-managed install guidance once the trust/update story is clear.

## Recommendation

1. **Short-term:** manually test `lmilojevicc/herdr-tab-rename` in Herdr. It is closest to the need and should immediately reduce numeric/misleading tab labels when cwd basename is enough.
2. **If app-first is still required:** implement a small dots-owned Herdr plugin or fork of `herdr-tab-rename` that adds process detection via `pane.process_info` before falling back to cwd basename.
3. **Do not treat this as a simple dots config change.** Herdr's config surface does not expose a dynamic title template, so a config-only PR would be misleading.
4. **Before dots installs any plugin automatically:** decide the trust/update model. Herdr plugins are unsandboxed user-level executables, so dots should either vendor/review the plugin or keep installation opt-in.

## Risks and gaps

- **Foreground process classification:** shells, wrappers, package runners, and editors can be ambiguous. For example, `pnpm develop` may appear as `pnpm`, `node`, or a shell depending on platform/process group visibility.
- **Manual rename semantics:** users still need a way to lock a tab name intentionally. The existing plugin's manual-rename lock behavior is a useful pattern.
- **Polling vs events:** the existing cwd plugin polls every 500ms. A custom plugin should evaluate whether Herdr event subscriptions can reduce polling, but `foreground_cwd`/process changes may still require periodic refresh.
- **Security/trust:** third-party plugins are not sandboxed or reviewed by Herdr.
- **Distribution:** if dots manages a plugin, it needs a reviewed install path, version pinning/update policy, and sandboxed validation that never writes to the operator's real home config.
