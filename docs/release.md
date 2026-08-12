# Release process

Tabby's release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts, SHA-256 checksums, and a Homebrew formula for Apple Silicon macOS. The canonical Herdr-managed adapter consumes the archive and checksum directly; Homebrew remains an alternative adapter.

## User install flow

User-facing install, verification, trust-model, stop, uninstall, and rollback instructions live in [`docs/install.md`](install.md). The primary install command is:

```sh
herdr plugin install yersonargotev/tabby
```

The canonical manifest runs `python3 scripts/install-herdr-plugin.py` before registration. The installer derives the release tag from the manifest version and consumes `tabby-aarch64-apple-darwin.tar.xz` plus its `.sha256` sidecar. Release validation must keep the Cargo package version, both manifest versions, tag, archive name, and checksum aligned. The Homebrew formula installs the `tabby` binary under the package `bin` directory and installs `packaging/herdr/herdr-plugin.toml` as `share/tabby/herdr-plugin.toml`; manifest validation requires identical product semantics and permits only the build declaration and executable paths to differ.

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
python3 -m unittest discover -s scripts/tests
python3 scripts/install-herdr-plugin.py --plugin-root /path/to/temporary-clean-checkout
dist plan
```

On macOS with Herdr 0.8.0 installed, also run `python3 scripts/herdr_lifecycle_harness.py`. Review its sanitized transcript and the recorded coverage in [`docs/evidence/issue-71-herdr-0.8-lifecycle.md`](evidence/issue-71-herdr-0.8-lifecycle.md) before tagging.

Review `.github/workflows/release.yml` and the generated Homebrew formula output before pushing the first release tag.
