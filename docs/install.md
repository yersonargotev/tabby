# Install Tabby with Homebrew

Use this guide to install the released Tabby Herdr plugin from the approved Homebrew tap, register it with Herdr, verify what Herdr will run, and remove or roll back the install.

## Release install

Install the released package from the approved tap:

```sh
brew install yersonargotev/tap/tabby
```

Register, or refresh, the Homebrew-managed plugin directory with Herdr:

```sh
tabby install
```

This is the v1 release path. Homebrew installs the `tabby` binary and release manifest; registration remains an explicit user command. `tabby install` idempotently relinks the shipped manifest and ensures the current Herdr Session is owned by the installed Session Runtime. When another Tabby binary owns it, the authenticated control endpoint performs a cooperative handoff; Tabby never kills an owner by PID.

Do not use `herdr plugin install yersonargotev/tabby` for the v1 release path. The Herdr marketplace/GitHub install path is intentionally not part of v1.

## Verify the install

Check the CLI is the released binary:

```sh
tabby --help
```

Expected output:

```text
Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install|unlock-focused|unlock-all|repair-state --discard|forget-session>
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

The report names the targeted Herdr Session and socket; shows registration; reports authoritative lease/runtime state, version, binary, launch, last evaluation/failure, and next cycle; shows the focused tab candidate; and counts locks, baselines, and unresolved intents. It never starts, repairs, hands off, unlocks, or renames anything.

## Use Tabby in Herdr

Tabby refreshes labels through one Session Runtime per Herdr Session. Herdr 0.8 starts it for new/restored sessions; manual commands cross the same Startup Gate. A manual refresh delivers a trigger to that owner:

```sh
tabby refresh
herdr plugin action invoke refresh --plugin yersonargotev.tabby
```

The runtime receives manifest hooks instead of opening `events.subscribe`. It suppresses inspection/rename during the 1000 ms Focus Quiet Window, evaluates the focused tab every five seconds, and uses at most three 500 ms samples per bounded attempt.

User-edited labels are treated as manual locks after Tabby has established a plugin label baseline. To clear locks from Herdr actions or the CLI:

```sh
herdr plugin action invoke unlock-focused --plugin yersonargotev.tabby
herdr plugin action invoke unlock-all --plugin yersonargotev.tabby
```

Each unlock also clears the associated plugin-label baseline. Unlock commands are serialized through the Ready owner and schedule a fresh eligible evaluation.

Expected successful `unlock-all` output:

```text
tabby unlock-all: cleared persisted manual locks
```

## Trust model

Herdr plugins run their configured commands as normal user code on your machine. Installing and linking Tabby means you trust the `tabby` binary from `yersonargotev/tap/tabby` and the Herdr manifest installed with that package.

The v1 release path is intentionally explicit:

- Homebrew installs files only; there is no silent Homebrew postinstall that registers or starts the plugin.
- `tabby install` is the separate opt-in registration/start step and may perform authenticated cooperative handoff.
- One lifetime lease keyed by lossless canonical socket identity prevents overlapping owners.
- Tabby does not silently auto-update. After `brew upgrade tabby`, run `tabby install` to refresh registration and runtime ownership.
- Tabby stores per-session locks, baselines, and rename intents in Herdr's plugin-owned state directory. You can locate its base with:

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

Optional: while Herdr is still available and the selected Session Runtime is stopped, explicitly forget that session's retained state before unlinking:

```sh
tabby forget-session
```

To roll back from the Homebrew release install to the local development link, keep the flows separate:

```sh
herdr plugin unlink yersonargotev.tabby
brew uninstall tabby
cargo build
herdr plugin link .
```
