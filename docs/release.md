# Release process

Tabby's v1 release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts, SHA-256 checksums, and a Homebrew formula for Apple Silicon macOS.

## User install flow

User-facing install, verification, trust-model, stop, uninstall, and rollback instructions live in [`docs/install.md`](install.md). The v1 release path remains:

```sh
brew install yersonargotev/tap/tabby
herdr plugin link "$(brew --prefix tabby)/share/tabby"
```

The Homebrew formula installs the `tabby` binary under the package `bin` directory and installs `packaging/herdr/herdr-plugin.toml` as `share/tabby/herdr-plugin.toml`. The release manifest invokes `../../bin/tabby` relative to `share/tabby`, keeping Herdr actions tied to the binary installed by the same Homebrew package.

## Tap validation

Validated on 2026-07-08 with `gh repo view yersonargotev/homebrew-tap --json nameWithOwner,visibility,isArchived,url,defaultBranchRef,pushedAt`: the tap exists at <https://github.com/yersonargotev/homebrew-tap>, is public, is not archived, and uses `main` as its default branch.

## Required release setup

- The tap repository `yersonargotev/homebrew-tap` must exist and be writable by the release workflow.
- Configure a GitHub Actions secret named `HOMEBREW_TAP_TOKEN` on `yersonargotev/tabby` before publishing the first release tag.
- `HOMEBREW_TAP_TOKEN` must be a GitHub token with write access to `yersonargotev/homebrew-tap` so `dist` can commit the generated formula.
- Do not create or rotate this secret from automation without explicit operator confirmation.

## Local verification before tagging

```sh
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 scripts/check-herdr-manifests.py
dist plan
```

Review `.github/workflows/release.yml` and the generated Homebrew formula output before pushing the first release tag.
