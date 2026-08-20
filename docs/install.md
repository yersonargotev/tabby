# Install Tabby

Use this guide to install the released Tabby plugin through Herdr on Apple Silicon macOS. Homebrew remains an optional alternative, not a prerequisite.

## Marketplace discovery and Herdr-managed install

Tabby is discoverable in the [Herdr community marketplace](https://herdr.dev/plugins/)
because its public GitHub repository carries the `herdr-plugin` topic. The
marketplace is an automatic community index: a listing is not a security review
or endorsement by Herdr. Review the repository, manifest, build command, and
trust surface below before installing.

The marketplace points to `yersonargotev/tabby`; its install action uses the
same GitHub-managed command as a direct installation:

```sh
herdr plugin install yersonargotev/tabby
```

Herdr shows the manifest build command in its trust preview, checks out the source into its managed plugin directory, and runs the command before registration. Tabby's installer reads the requested version from `herdr-plugin.toml`, downloads `tabby-aarch64-apple-darwin.tar.xz` and its published checksum from that version's GitHub Release, verifies SHA-256 before extraction, and atomically switches `.herdr/bin/tabby` to a private per-install executable instance. This keeps the manifest command stable while giving each successful reinstall a distinct identity for Cooperative Runtime Handoff. A failure leaves the plugin unregistered and does not replace a previously complete executable with a partial file.

The supported path requires Apple Silicon macOS, Herdr 0.8.0/protocol 19 or newer, network access to `github.com`, and Python 3.9 or newer with standard-library HTTPS, SHA-256, and xz/tar support. The protocol number belongs to Herdr's binary client/server wire format and is not Tabby's compatibility boundary. Before becoming Ready, Tabby uses the plugin host-provided `HERDR_BIN_PATH` to validate the required JSON shapes for `session.snapshot`, `pane.process_info`, and `tab.rename`, then performs only read-only live probes against the exact selected session socket. Additive fields and later wire protocols are accepted when this contract validates; missing or incompatible requirements fail closed with an actionable runtime diagnostic. The `0.8.0 / 19` and `0.8.2 / 20` pairs remain release evidence rather than a permanent allowlist. The supported path does not require Rust. Only when the matching release archive returns HTTP 404 may the installer run `cargo build --release --locked`. That fallback requires the current stable Rust toolchain, with both `cargo` and `rustc` available on `PATH`; install it through [rustup](https://rustup.rs/) and keep `Cargo.lock` unchanged. Intel macOS, Linux, and Windows are rejected rather than receiving an untested binary.

Start Tabby immediately in the current Herdr Session:

```sh
herdr plugin action invoke start --plugin yersonargotev.tabby
```

To update or reinstall the Herdr-managed plugin, run the install command again, explicitly activate the registered binary, and verify the selected Session Runtime:

```sh
herdr plugin install yersonargotev/tabby
herdr plugin action invoke start --plugin yersonargotev.tabby
plugin_root="$(herdr plugin list --plugin yersonargotev.tabby --json | jq -r '.result.plugins[0].plugin_root')"
"$plugin_root/.herdr/bin/tabby" status
```

The repeated install updates the managed checkout and rebuilds the same canonical executable contract. The `start` action cooperatively hands ownership to that registered binary if another released distribution currently owns the Session Runtime.

## Homebrew alternative

Install the released package from the approved tap:

```sh
brew install yersonargotev/tap/tabby
```

Register, or refresh, the Homebrew-managed plugin directory with Herdr:

```sh
tabby install
```

Homebrew installs the `tabby` binary and its alternative release manifest; registration remains an explicit user command. `tabby install` idempotently relinks that manifest and ensures the current Herdr Session is owned by the installed Session Runtime. When another Tabby binary owns it, the authenticated control endpoint performs a cooperative handoff; Tabby never kills an owner by PID.

## Verify the install

Check the CLI is the released binary:

```sh
tabby --help
```

Expected output:

```text
Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install|config <path|check|reload>|unlock-focused|unlock-all|repair-state --discard|forget-session>
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
config-check ../../bin/tabby config check
config-path ../../bin/tabby config path
config-reload ../../bin/tabby config reload
start ../../bin/tabby start
unlock-all ../../bin/tabby unlock-all
unlock-focused ../../bin/tabby unlock-focused
```

The important checks are:

- `enabled` is `true`.
- `plugin_root` is under Homebrew's current `tabby` Cellar version, ending in `share/tabby`.
- actions run `../../bin/tabby`, so Herdr invokes the binary installed by the same Homebrew package.
- the explicit `start` action runs `tabby start` and activates the binary registered by that manifest;
- startup and lifecycle hooks run `tabby ensure-started` or the corresponding signal command without replacing a validated owner;
- `workspace.created` and `tab.created` run `tabby signal-created`, which recovers a missing owner through the Startup Gate but does not reset an already Ready owner's quiet window;
- `pane.focused` runs `tabby signal-focus`, which delivers the focus trigger to the Session Runtime control endpoint.

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

### Configure the Label Policy

Locate the file without creating it:

```sh
tabby config path
```

Create `config.toml` at that path with at least `version = 1`. Every other field is optional; omitted fields preserve the built-in defaults. For example:

```toml
version = 1

[labels]
max_length = 32
max_display_width = 32
cwd_components = 1

[labels.prefixes]
"lazygit" = "git: "
"pnpm dev" = "run: "

[commands]
additional_significant = ["yazi", "btop"]
additional_ignored = ["python", "sleep"]

[commands.runners]
pnpm = ["dev", "test", "lint"]
cargo = ["run", "test", "watch"]

[commands.aliases]
"lazygit" = "git"
"pnpm dev" = "dev"

[directories.aliases]
"~/dev/tabby" = "tabby"
"/Users/me/work/customer-api" = "api"
```

The built-ins remain Significant Commands `nvim`, `lazygit`, `codex`, and `claude`; runner pairs `pnpm dev`, `npm test`, `go test`, and `cargo run`; ignored shells/wrappers including `zsh`, `bash`, `tmux`, `env`, and `sudo`; `max_length = 32`; and `cwd_components = 1`. The supported ranges are 1–128 Unicode scalars for `max_length`, 1–256 terminal display cells for optional `max_display_width`, and 1–8 trailing path components for `cwd_components`. Directory aliases apply only when no Significant Command was classified.

`labels.prefixes` keys are classified Significant Command or runner/subcommand candidates, never raw process fields. Tabby applies command aliases first, then adds the prefix keyed by the original classified candidate, then performs one final truncation. Prefixes have no defaults: omit the table to retain plain labels, and Tabby ships no icon or Nerd Font mapping.

`max_length` keeps its original Unicode-scalar meaning. `max_display_width` is an independent optional bound using [`unicode-width` 0.2.2](https://docs.rs/unicode-width/0.2.2/unicode_width/) and its Unicode 17.0.0 tables. Tabby uses the crate's conservative non-CJK width (`width`): ASCII is one cell, CJK wide characters are two, combining sequences are kept together, fully-qualified emoji ZWJ sequences are treated as two, and East Asian ambiguous characters are narrow. Private-use characters are measured conservatively by those tables, but actual rendering remains font- and terminal-dependent; this is a bound, not a promise of font-perfect display width.

Directory selectors are exact lexical paths. They accept absolute paths and `~/...`, with `~` expanded while loading `config.toml`. Tabby uses `foreground_cwd` before pane `cwd`, collapses lexical `.` and `..` components, and does not access the filesystem. Therefore aliases work for restored or deleted paths, while symlink spellings remain distinct. Globs, prefix matching, case folding, filesystem canonicalization, and automatic symlink resolution are not supported.

Validate and activate changes with:

```sh
tabby config check
tabby config reload
tabby status
```

Unknown fields and unsupported versions are errors. Empty or unsafe labels, unknown prefix candidates, duplicate normalized directory selectors, duplicate/contradictory command entries, and out-of-range values are also rejected with their field name. A missing file means built-in defaults. A rejected reload keeps the last valid active policy and appears in `tabby status`; an invalid initial file prevents the Session Runtime from becoming Ready. Configuration cannot change runtime cadence, quiet windows, stability requirements, deadlines, manual-lock behavior, ownership, leases, or persistence.

### Configure Session profiles

The global policy is used when no selector matches. A selected profile is compiled only from built-in defaults and its `extends` chain; it does not overlay the global policy. Child scalar fields override inherited values, command lists add entries, and duplicate map keys across inheritance are rejected.

```toml
[profiles.work]
extends = "engineering"

[profiles.work.labels]
cwd_components = 2

[profiles.engineering.commands]
additional_significant = ["yazi"]

[[session_selectors]]
profile = "work"
identity = "/Users/me/.config/herdr/sessions/work/herdr.sock"

[[session_selectors]]
profile = "personal"
named_session = "personal"
```

Every selector needs exactly one of `identity`, `identity_hex`, or `named_session`. `identity` is an absolute socket path. `identity_hex` is the lowercase, lossless byte encoding shown by `tabby status`, for identities that cannot be represented as UTF-8. The named shorthand derives `sessions/<name>/herdr.sock` from the receiving runtime's documented socket root and is rejected for a custom socket layout. Every form matches Tabby's exact resolved Session Identity. Reload is local to that Ready runtime only; it does not provide reload-all and does not change persisted locks, baselines, intents, leases, ownership, or identity. Status shows the selected profile and policy source and preserves the last valid values after a rejected reload.

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

## Detach, stop, and restore

Closing or detaching a Herdr client does not stop its server. The Ready Session Runtime remains owned and continues one bounded focused-tab evaluation every five seconds.

A Session Stop is different: Tabby ends only after the canonical Herdr socket disappears or reports that no server is listening. An RPC timeout or ambiguous transport error ends one evaluation but does not prove a stop. Locks, baselines, and unresolved rename intents remain stored for that Session Identity.

When Herdr restores the session, the manifest's `[[startup]]` command crosses the Startup Gate. Exactly one new Session Runtime becomes Ready and begins its initial evaluation after the Focus Quiet Window. Run `tabby status` with that session selected to inspect the new launch identity and retained state.

## Diagnose and repair state

Start with the read-only command:

```sh
tabby status
```

`Absent` means no owner holds the selected session lease. `Starting` means the Startup Gate has not yet proved readiness. `Ready` reports the authoritative lease owner. `Faulted` includes an actionable failure and performs no automatic tab mutation.

If status reports a State Integrity Fault, inspect the preserved evidence first. To explicitly archive the invalid bytes and create clean session state through the owner, run:

```sh
tabby repair-state --discard
```

Repair is never implicit and `tabby status` never performs it. To remove valid retained state instead, stop the exact selected Herdr Session, preserve its `HERDR_SOCKET_PATH` as the Session Identity, and run `tabby forget-session`. Tabby refuses to forget a running session or a different identity.

## Trust model

Herdr plugins execute unsandboxed as normal user code on your machine. Marketplace discovery does not change that trust boundary. A GitHub-managed install runs `python3 scripts/install-herdr-plugin.py` from the reviewed checkout with network access, then executes the verified Tabby binary. The installer contacts only the versioned release URLs under `github.com/yersonargotev/tabby`, downloads an archive and checksum, and writes the canonical executable inside the managed plugin root. The optional source fallback executes the local Rust `cargo` toolchain. Homebrew users instead trust the binary and manifest from `yersonargotev/tap/tabby`.

The release paths are intentionally explicit:

- Herdr previews the GitHub-managed build command and aborts before registration on any build failure.
- Downloaded bytes are never extracted or executed until their published SHA-256 checksum is present, well-formed, and matches.
- Homebrew installs files only; there is no silent Homebrew postinstall that registers or starts the plugin.
- `tabby install` is the separate opt-in registration/start step and may perform authenticated cooperative handoff.
- One lifetime lease keyed by lossless canonical socket identity prevents overlapping owners.
- Tabby does not silently auto-update. After `brew upgrade tabby`, run `tabby install` to refresh registration and runtime ownership.
- Tabby stores per-session locks, baselines, and rename intents in Herdr's plugin-owned state directory. You can locate its base with:

```sh
herdr plugin config-dir yersonargotev.tabby
```

Herdr supplies that plugin-owned configuration/state location, with XDG
fallbacks when needed. Tabby stores Session-Scoped Tab State there, never in the
marketplace index, managed checkout, or Homebrew package. Registration and
runtime activation remain explicit: install first, then invoke the `start`
action shown above.

## Release evidence

The recorded production proof uses release
[`v0.1.13`](https://github.com/yersonargotev/tabby/releases/tag/v0.1.13). Its
[native Herdr install evidence](https://github.com/yersonargotev/tabby/blob/main/docs/evidence/issue-79-herdr-native-release.md)
records checksum-verified installation, registration, explicit activation,
status, lifecycle behavior, uninstall, retained state, and isolated cleanup.
The earlier [Herdr 0.8 lifecycle evidence](https://github.com/yersonargotev/tabby/blob/main/docs/evidence/issue-71-herdr-0.8-lifecycle.md)
documents the underlying Session Runtime lifecycle and its coverage limits.

## Migrate between released distributions

Session-Scoped Tab State belongs to the Herdr Session rather than either installation root, so locks, baselines, and rename intents survive migration. In both directions, explicit activation uses Cooperative Runtime Handoff through the authenticated control endpoint: the proven owner releases its lease before the replacement starts, and Tabby never kills an owner by PID.

To migrate from Homebrew to the primary Herdr-managed installation:

```sh
herdr plugin unlink yersonargotev.tabby
herdr plugin install yersonargotev/tabby
herdr plugin action invoke start --plugin yersonargotev.tabby
plugin_root="$(herdr plugin list --plugin yersonargotev.tabby --json | jq -r '.result.plugins[0].plugin_root')"
"$plugin_root/.herdr/bin/tabby" status
brew uninstall tabby
```

Only uninstall Homebrew after status identifies the managed plugin root and `.herdr/bin/tabby` as the Ready owner. If activation fails, the prior proven owner remains intact; relink the Homebrew adapter with `tabby install` before retrying.

To migrate from a Herdr-managed installation to Homebrew:

```sh
brew install yersonargotev/tap/tabby
tabby install
tabby status
```

`tabby install` unregisters the prior adapter, links Homebrew's current `share/tabby` manifest, and cooperatively activates the Homebrew binary for the selected Herdr Session. After status shows the Homebrew registered command and Ready owner, the managed checkout is no longer registered. If you need to remove retained managed files separately, follow the Herdr version's documented plugin cleanup command only after verifying the Homebrew owner.

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

Remove a GitHub-managed checkout and its registration with:

```sh
herdr plugin uninstall yersonargotev.tabby
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
python3 scripts/prepare-herdr-plugin.py
herdr plugin link .
```

Local linking uses the same `.herdr/bin/tabby` plugin-root executable contract as the production-shaped root manifest. Herdr does not run build commands for linked plugins, so prepare that path explicitly after every debug build.

To roll back the Herdr-managed path to an earlier released version, check out the desired release tag in a reviewed local clone, prepare its verified plugin-root binary, and link it as an explicit local rollback:

```sh
git checkout v<version>
python3 scripts/install-herdr-plugin.py
herdr plugin uninstall yersonargotev.tabby
herdr plugin link .
herdr plugin action invoke start --plugin yersonargotev.tabby
```

This rollback is intentionally distinct from routine managed updates and local development. Review the selected tag and its manifest build command before running it; use the migration procedures above when changing only between the current Herdr-managed and Homebrew releases.
