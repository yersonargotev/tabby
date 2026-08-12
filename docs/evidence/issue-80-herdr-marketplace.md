# Issue 80 Herdr marketplace evidence

Date: 2026-08-11 (America/Bogota), 2026-08-12 UTC
Host contract: Apple Silicon macOS, Herdr 0.8.0, protocol 19
Tested release: `v0.1.13`
Result: passed

## Marketplace discovery

The public repository has the `herdr-plugin` topic and the repository
description `Herdr plugin that labels focused tabs with a Significant Command
or Working Directory Basename.`

The public marketplace snapshot generated at `2026-08-12T04:00:45.109Z`
resolved one repository entry with these production fields:

```json
{
  "fullName": "yersonargotev/tabby",
  "url": "https://github.com/yersonargotev/tabby",
  "topics": ["herdr-plugin"],
  "manifests": [{
    "path": "herdr-plugin.toml",
    "id": "yersonargotev.tabby",
    "name": "Tabby",
    "version": "0.1.13",
    "minHerdrVersion": "0.8.0",
    "description": "Labels the focused tab with a Significant Command or Working Directory Basename.",
    "platforms": ["macos"]
  }]
}
```

This was read from `https://assets.herdr.dev/plugins/index.json`, the snapshot
used by `https://herdr.dev/plugins/`. The marketplace links to the repository;
it does not define a separate installation protocol. Herdr 0.8.0 accepts that
source through `herdr plugin install yersonargotev/tabby`.

## Final isolated smoke check

After publishing the repository metadata, the release harness ran again in a
fresh temporary environment. It redirected `HOME`, all XDG roots, `TMPDIR`,
`HERDR_CONFIG_PATH`, Herdr sockets, and Session-Scoped Tab State beneath the
temporary root, removed inherited `HERDR_*` variables, and excluded `cargo` and
`rustc` from `PATH`. Cleanup stopped the isolated Herdr server and removed the
temporary root without reading or writing the operator's Herdr environment.

The run passed all 38 transcript steps. In particular, it:

- resolved `yersonargotev/tabby` at release `v0.1.13` and installed it through
  Herdr's GitHub-managed `plugin install` flow;
- registered `yersonargotev.tabby` with the production plugin root, build,
  startup, events, actions, and canonical `.herdr/bin/tabby` command;
- invoked the explicit `start` action and observed a Ready Session Runtime with
  version `0.1.13` and no warnings;
- read Runtime Status twice without changing the owner or Session-Scoped Tab
  State;
- uninstalled the plugin, observed an empty registration list, stopped the
  server, and confirmed the socket listener had ended.

The sanitized live transcript had SHA-256
`80cedbc488d9a82293e5d532b76f9059198700ae8d24fb844541eeb42bcdb991`.
The checked-in [native release evidence](issue-79-herdr-native-release.md) and
its [sanitized transcript](issue-79-herdr-native-release.jsonl) document the
same release's complete installation and lifecycle assertions in reproducible
form.

## Trust boundary

Marketplace inclusion is automatic discovery from the GitHub topic, not a
security review or endorsement. Installation executes the manifest's build and
runtime commands unsandboxed as the current user. The installation guide
documents the build-time release download and SHA-256 verification,
configuration and state locations, explicit activation, and Homebrew as an
optional alternative rather than a prerequisite.
