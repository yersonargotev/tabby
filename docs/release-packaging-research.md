# Release Packaging Research

Date: 2026-07-08
Status: research note for GitHub issue #1; not a final decision.

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

- https://github.com/axodotdev/cargo-dist/blob/main/book/src/quickstart/rust.md
- https://github.com/axodotdev/cargo-dist/blob/main/book/src/installers/homebrew.md
- https://github.com/axodotdev/cargo-dist/blob/main/book/src/reference/config.md

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

## Recommendation to confirm

Use Option B: `dist` / `cargo-dist` generated releases with Homebrew tap publishing.

Proposed initial release policy:

- Create a separate tap repo, likely `yersonargotev/homebrew-tap` unless we want a Tabby-specific tap.
- Ship macOS arm64 first if we want the smallest safe release, or macOS arm64+x86_64 if cross-target CI is low-friction.
- Treat standalone install scripts as secondary; the public v1 install path should be `brew tap ... && brew install tabby` unless a script is still needed for Herdr plugin registration.
- Review the generated workflow and formula before the first tag.

## Open questions for grilling

1. Should the tap be general (`yersonargotev/homebrew-tap`) or project-specific (`yersonargotev/homebrew-tabby`)?
2. Should first release targets be macOS arm64 only or both macOS arm64 and x86_64?
3. Is `brew install tabby` enough, or must release packaging also automate Herdr plugin registration?
4. Should there be an auditable install script in v1, or should docs plus Homebrew be the only install surface?
