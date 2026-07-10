# Install Tabby with Homebrew

Use this guide to install the released Tabby Herdr plugin from the approved Homebrew tap, register it with Herdr, verify what Herdr will run, and remove or roll back the install.

## Release install

Install the released package from the approved tap:

```sh
brew install yersonargotev/tap/tabby
```

Register, or refresh, the Homebrew-managed plugin directory with Herdr:

```sh
tabby install --start
```

This is the v1 release path. Homebrew installs the `tabby` binary and the release Herdr manifest; Herdr registration remains an explicit user command. `tabby install` is intentionally idempotent: it asks Herdr to unlink any existing `yersonargotev.tabby` registration, then links the manifest shipped with the currently running Homebrew package. Plain `tabby install` remains registration-only; `tabby install --start` also ensures exactly one Hybrid Session Refresher is running for the current Herdr Session.

Startup records the resolved executable target when it spawns a refresher and validates that identity against the currently running `tabby` executable. This detects Homebrew upgrades even when the stable `/opt/homebrew/bin/tabby` symlink is reused for a new Cellar version. If the current Herdr Session still has a refresher from a different local checkout or Homebrew version—or legacy metadata cannot prove its launch-time binary—`ensure-started` refuses to accept it and reports the recorded PID and paths. Stop only that reported refresher with `kill <pid>`, then rerun `tabby install --start`. Tabby does not terminate it automatically, avoiding the risk of killing an unrelated reused PID.

Do not use `herdr plugin install yersonargotev/tabby` for the v1 release path. The Herdr marketplace/GitHub install path is intentionally not part of v1.

## Verify the install

Check the CLI is the released binary:

```sh
tabby --help
```

Expected output:

```text
Usage: tabby <status|refresh|start|ensure-started|install [--start]|unlock-focused|unlock-all>
```

Check Homebrew's install prefix:

```sh
brew --prefix tabby
```

Expected output on Apple Silicon Homebrew installs:

```text
/opt/homebrew/opt/tabby
```

Check Herdr registered the Homebrew plugin, not the local development checkout:

```sh
herdr plugin list --plugin yersonargotev.tabby --json \
  | jq -r '.result.plugins[0] | .enabled, .plugin_root, (.actions[] | "\(.id) \(.command | join(" "))")'
```

Expected output for the current installed version is shaped like:

```text
true
/opt/homebrew/Cellar/tabby/<version>/share/tabby
refresh ../../bin/tabby refresh
start ../../bin/tabby ensure-started
unlock-all ../../bin/tabby unlock-all
unlock-focused ../../bin/tabby unlock-focused
```

The important checks are:

- `enabled` is `true`.
- `plugin_root` is under Homebrew's current `tabby` Cellar version, ending in `share/tabby`.
- actions run `../../bin/tabby`, so Herdr invokes the binary installed by the same Homebrew package.
- the `start` action runs `tabby ensure-started`; creation event hooks also run `ensure-started`; focus events are handled inside the running refresher, not by manifest hooks.

For a single read-only diagnostic covering these checks plus runtime state, run:

```sh
tabby status
```

The report names the targeted Herdr Session and socket; shows the registered manifest and resolved command paths; checks the recorded refresher PID, version, binary, and socket; shows the focused workspace/tab/pane and current Tab Label Candidate; summarizes Manually Locked Tabs; and highlights suspicious focused-tab baselines or recent failed/lock-skipped plugin actions. It never starts, stops, unlocks, or renames anything. Any recovery command appears only under `Suggested fixes`.

## Use Tabby in Herdr

Tabby refreshes labels through one Hybrid Session Refresher per Herdr Session. Start it with `tabby install --start`, `tabby ensure-started`, or Herdr’s `Start Tabby` action. The manual one-shot refresh remains available:

```sh
tabby refresh
herdr plugin action invoke refresh --plugin yersonargotev.tabby
```

The refresher subscribes to Herdr focus/create events, suppresses `pane.process_info` and `tab.rename` during the 1000 ms Focus Quiet Window, then inspects only the focused tab every 500 ms outside the window. The `tabby refresh` command is still one-shot and exits after at most one focused-tab rename.

User-edited labels are treated as manual locks after Tabby has established a plugin label baseline. To clear locks from Herdr actions or the CLI:

```sh
herdr plugin action invoke unlock-focused --plugin yersonargotev.tabby
herdr plugin action invoke unlock-all --plugin yersonargotev.tabby
```

Each unlock also clears the associated plugin-label baseline for the unlocked tab. This prevents the next refresh from immediately recreating the same lock, and the running Hybrid Session Refresher observes the persisted change before its next refresh outside the Focus Quiet Window.

Expected successful `unlock-all` output:

```text
tabby unlock-all: cleared persisted manual locks
```

## Trust model

Herdr plugins run their configured commands as normal user code on your machine. Installing and linking Tabby means you trust the `tabby` binary from `yersonargotev/tap/tabby` and the Herdr manifest installed with that package.

The v1 release path is intentionally explicit:

- Homebrew installs files only; there is no silent Homebrew postinstall that registers or starts the plugin.
- `tabby install` is the separate opt-in registration step. It is a small wrapper around `herdr plugin unlink yersonargotev.tabby` followed by `herdr plugin link <current package>/share/tabby`; add `--start` to start the current session refresher explicitly.
- Automatic label refreshes come from one long-running Hybrid Session Refresher per Herdr Session; duplicate prevention is keyed by the Herdr socket identity.
- Tabby does not silently auto-update. Updates happen through Homebrew, for example `brew upgrade tabby`; run `tabby install --start` after upgrades so Herdr points at the current Homebrew Cellar path and startup verifies that the current session refresher uses the same binary.
- Tabby stores its lock state as `locks.json` in Herdr's plugin-owned state/config directory. You can inspect that directory with:

```sh
herdr plugin config-dir yersonargotev.tabby
```

## Update or relink after Homebrew upgrades

Homebrew installs each version in a versioned Cellar directory and may remove old versions during cleanup. Herdr stores the resolved plugin root, so an old registration can point at a directory that no longer exists after `brew upgrade`.

Refresh Herdr after installing or upgrading Tabby:

```sh
brew upgrade yersonargotev/tap/tabby
tabby install
```

If you prefer the raw Herdr commands, the equivalent recovery is:

```sh
herdr plugin unlink yersonargotev.tabby || true
herdr plugin link "$(brew --prefix tabby)/share/tabby"
```

If Herdr returns `Error: Os { code: 2, kind: NotFound, message: "No such file or directory" }`
from `plugin link` or `plugin action invoke`, the shell may be carrying a stale
`HERDR_SOCKET_PATH` from a previous Herdr server process. Retry after letting
Herdr rediscover the current Herdr Session:

```sh
env -u HERDR_SOCKET_PATH tabby install
env -u HERDR_SOCKET_PATH herdr plugin action invoke refresh --plugin yersonargotev.tabby
```

## Disable, uninstall, or roll back

Disable the plugin without removing the Homebrew package:

```sh
herdr plugin disable yersonargotev.tabby
```

Unregister the Homebrew-linked plugin and uninstall the package:

```sh
herdr plugin unlink yersonargotev.tabby
brew uninstall tabby
```

Optional: remove Tabby's persisted lock state after unlinking if you do not want to keep manual-lock state for a future reinstall:

```sh
rm -f "$(herdr plugin config-dir yersonargotev.tabby)/locks.json"
```

To roll back from the Homebrew release install to the local development link, keep the flows separate:

```sh
herdr plugin unlink yersonargotev.tabby
brew uninstall tabby
cargo build
herdr plugin link .
```
