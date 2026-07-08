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

This is the v1 release path. Homebrew installs the `tabby` binary and the release Herdr manifest; Herdr registration remains an explicit user command. `tabby install` is intentionally idempotent: it asks Herdr to unlink any existing `yersonargotev.tabby` registration, then links the manifest shipped with the currently running Homebrew package.

Do not use `herdr plugin install yersonargotev/tabby` for the v1 release path. The Herdr marketplace/GitHub install path is intentionally not part of v1.

## Verify the install

Check the CLI is the released binary:

```sh
tabby --help
```

Expected output:

```text
Usage: tabby <daemon|start|install|unlock-focused|unlock-all>
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
start ../../bin/tabby start
unlock-all ../../bin/tabby unlock-all
unlock-focused ../../bin/tabby unlock-focused
```

The important checks are:

- `enabled` is `true`.
- `plugin_root` is under Homebrew's current `tabby` Cellar version, ending in `share/tabby`.
- actions run `../../bin/tabby`, so Herdr invokes the binary installed by the same Homebrew package.

## Start and use Tabby in Herdr

Start the daemon from Herdr by invoking the `Start Tabby daemon` action for plugin `yersonargotev.tabby`. From the CLI, the equivalent is:

```sh
herdr plugin action invoke start --plugin yersonargotev.tabby
```

The start action is long-running: it keeps `tabby start` active while Tabby watches Herdr tabs and applies automatic labels.

Manual labels are treated as locks. To clear locks from Herdr actions or the CLI:

```sh
herdr plugin action invoke unlock-focused --plugin yersonargotev.tabby
herdr plugin action invoke unlock-all --plugin yersonargotev.tabby
```

Expected successful `unlock-all` output:

```text
tabby unlock-all: cleared persisted manual locks
```

## Trust model

Herdr plugins run their configured commands as normal user code on your machine. Installing and linking Tabby means you trust the `tabby` binary from `yersonargotev/tap/tabby` and the Herdr manifest installed with that package.

The v1 release path is intentionally explicit:

- Homebrew installs files only; there is no silent Homebrew postinstall that registers or starts the plugin.
- `tabby install` is the separate opt-in registration step. It is a small wrapper around `herdr plugin unlink yersonargotev.tabby` followed by `herdr plugin link <current package>/share/tabby`.
- Tabby does not silently auto-update. Updates happen through Homebrew, for example `brew upgrade tabby`; run `tabby install` after upgrades so Herdr stops pointing at any old Homebrew Cellar path that cleanup removed.
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

## Stop, disable, uninstall, or roll back

Tabby does not provide a separate `tabby stop` command. To stop a running daemon action, close the running Herdr plugin action/pane if it is visible in Herdr, or terminate the `tabby start` process from a shell:

```sh
pgrep -af 'tabby start'
kill <pid>
```

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

The local-link development manifest at the repo root runs `target/debug/tabby`; the release manifest installed by Homebrew runs `../../bin/tabby` from the Homebrew package.
