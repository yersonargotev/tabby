# Release Packaging Research

Date: 2026-07-08
Status: resolved by ADR 0005; retained as supporting research for GitHub issue #1.

## Goal

Find a release path that takes Tabby from local-link development to a real user install flow, with Homebrew tap support as a hard requirement.

## Constraints from project docs

- macOS first.
- Release binaries before broader install docs.
- Checksums required.
- Install path must be auditable.
- No silent auto-update.
- Keep local-link development available while release packaging is added.

## Primary-source findings

### Homebrew tap requirements

Homebrew's tap documentation shows custom formulae being created inside a tap with `brew create ... --tap owner/homebrew-tap`, and Homebrew formula docs use `url` plus `sha256` checksums for reproducible source/resource downloads. Homebrew checksum deprecation docs require SHA-256 rather than MD5/SHA-1 for custom taps.

Sources:

- https://github.com/Homebrew/brew/blob/main/docs/How-to-Create-and-Maintain-a-Tap.md
- https://github.com/Homebrew/brew/blob/main/docs/Formula-Cookbook.md
- https://github.com/Homebrew/brew/blob/main/docs/Checksum_Deprecation.md

### `dist` / `cargo-dist`

`dist` (formerly `cargo-dist`) is purpose-built for Rust binary distribution. Official docs describe `dist init --yes` as the setup command that adds dist configuration and a GitHub Actions release workflow. Release automation is tag-driven: update the version, commit, push, tag (for example `v0.1.0`), and push tags to trigger release artifact creation.

For Homebrew, official dist docs support adding `homebrew` to installers and configuring a tap repository:

```toml
[workspace.metadata.dist]
installers = ["homebrew"]
tap = "owner/homebrew-tap"
publish-jobs = ["homebrew"]
```

The tap repository must already exist and the publishing token needs write access.

Sources:

- https://axodotdev.github.io/cargo-dist/book/quickstart/rust.html
- https://axodotdev.github.io/cargo-dist/book/installers/homebrew.html
- https://axodotdev.github.io/cargo-dist/book/artifacts/checksums.html
- https://axodotdev.github.io/cargo-dist/book/reference/config.html

### Herdr plugin installation model

Herdr plugins are directories with a `herdr-plugin.toml` manifest. `herdr plugin install owner/repo[/subdir...]` clones a GitHub-managed plugin checkout, runs supported build commands after preview/confirmation, and registers the plugin. `herdr plugin link /path/to/plugin` registers a local plugin directory without running build commands. Marketplace discovery is an automatic, unreviewed index of public GitHub repositories tagged with `herdr-plugin`; marketplace installation still uses `herdr plugin install owner/repo[/subdir...]`.

This means a Homebrew-installed binary alone is not enough to register Tabby as a Herdr plugin. For v1, the release package needs to install plugin assets and docs need to tell the user to explicitly link the Homebrew-managed plugin directory.

Sources:

- https://herdr.dev/docs/plugins/
- https://herdr.dev/docs/marketplace/

## Options

### Option A — Hand-written GitHub Actions + hand-maintained tap formula

Pros:

- Maximum control.
- No release generator dependency.

Cons:

- More bespoke release logic to audit and maintain.
- More chances for artifact names, checksums, and tap formula URLs to drift.
- Slower path to a reliable v1 release.

### Option B — `dist` / `cargo-dist` generated releases + Homebrew tap publishing

Pros:

- Directly supports Rust binaries, GitHub Releases, checksums/artifacts, and Homebrew tap publishing.
- Smaller amount of custom release code for Tabby to maintain.
- Fits tag-driven release flow.

Cons:

- Adds a release tool and generated workflow that must be reviewed.
- Requires creating and granting write access to the tap repository.
- Need to verify generated formula/action commands match Herdr plugin installation needs, not just CLI binary installation.

### Option C — Source-only Homebrew formula that builds Tabby on user machines

Pros:

- Simple tap formula in principle.
- Avoids managing binary artifacts at first.

Cons:

- Requires Rust toolchain on user machines.
- Slower install and more user-environment failure modes.
- Does not satisfy the project goal of release binaries first.

## Resolved decision

See [ADR 0005](adr/0005-use-dist-and-homebrew-managed-plugin-link-for-release.md).

The accepted v1 release policy is:

- Use Option B: `dist` / `cargo-dist` generated releases with Homebrew tap publishing.
- Publish to the general tap repo `yersonargotev/homebrew-tap`.
- Ship Apple Silicon macOS only for the first release (`aarch64-apple-darwin`). Linux compatibility is a future direction, not part of v1 packaging.
- Install the release binary and Herdr plugin assets through Homebrew, then require an explicit user-run registration command such as `herdr plugin link "$(brew --prefix tabby)/share/tabby"`.
- Do not list Tabby in the Herdr marketplace for v1 and do not support `herdr plugin install yersonargotev/tabby` yet.
- Do not ship a standalone install script for v1.
- Keep separate Herdr manifests: the root `herdr-plugin.toml` remains the local-link development manifest, while a release manifest is installed under the Homebrew package prefix.
- Review the generated release workflow and formula before the first tag.

## Grilling outcomes

1. Use the general tap `yersonargotev/homebrew-tap`.
2. First release target is Apple Silicon macOS only.
3. `brew install` is not enough by itself: Homebrew installs the binary and plugin assets, and the user explicitly links the Homebrew-managed plugin directory with Herdr.
4. No standalone install script in v1; docs plus explicit commands are the public install surface.
