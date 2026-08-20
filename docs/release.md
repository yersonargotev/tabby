# Release process

Tabby's release path uses `dist`/`cargo-dist` to publish GitHub Release artifacts, SHA-256 checksums, and a Homebrew formula for Apple Silicon macOS. The canonical Herdr-managed adapter consumes the archive and checksum directly; Homebrew remains an alternative adapter.

## User install flow

User-facing install, verification, trust-model, stop, uninstall, and rollback instructions live in [`docs/install.md`](install.md). The primary install command is:

```sh
herdr plugin install yersonargotev/tabby
```

The canonical manifest runs `python3 scripts/install-herdr-plugin.py` before registration. The installer derives the release tag from the manifest version and consumes `tabby-aarch64-apple-darwin.tar.xz` plus its `.sha256` sidecar. Release validation keeps the Cargo package version, both manifest versions, exact `v<version>` tag, archive name, target, checksum relationship, and checksum bytes aligned. The Homebrew formula installs the `tabby` binary under the package `bin` directory and installs `packaging/herdr/herdr-plugin.toml` as `share/tabby/herdr-plugin.toml`; manifest validation requires identical product semantics and permits only the build declaration and executable paths to differ.

## CI release contract

Pull requests run `dist plan`, validate its output through `scripts/check-release-contract.py`, build the production Apple Silicon archive with `pr-run-mode = "upload"`, and verify the generated sidecar against the archive bytes. Workflow artifacts are retained for inspection, but the publish and Homebrew jobs remain gated to a release tag.

Release tags run the same checks with the explicit Git tag. Planning fails unless every version is identical and cargo-dist declares both artifacts consumed by the native installer:

- `tabby-aarch64-apple-darwin.tar.xz`
- `tabby-aarch64-apple-darwin.tar.xz.sha256`

The local build job then requires one well-formed SHA-256 entry naming that exact archive and verifies its digest before any host or publish job can run. `scripts/check-herdr-manifests.py` separately preserves behavioral parity across the canonical GitHub-managed/local adapter and the Homebrew adapter.

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
dist plan --output-format=json > plan-dist-manifest.json
release_tag="v$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')"
python3 scripts/check-release-contract.py --dist-manifest plan-dist-manifest.json --tag "$release_tag"
```

On Apple Silicon macOS, run the lifecycle harness against both required Herdr evidence pairs before tagging. These pairs prove the required JSON contract across two adjacent binary wire protocols; they are not a runtime allowlist. Select each exact binary through `PATH` and pass explicit expectations so the harness cannot exercise a different installation accidentally:

```sh
PATH="/path/to/herdr-0.8.0/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  python3 scripts/herdr_lifecycle_harness.py \
  --expected-herdr-version 0.8.0 \
  --expected-herdr-protocol 19

PATH="/path/to/herdr-0.8.2/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  python3 scripts/herdr_lifecycle_harness.py \
  --expected-herdr-version 0.8.2 \
  --expected-herdr-protocol 20
```

The `0.8.0 / 19` pair remains the harness default for local convenience, but the release proof must pass both explicit commands. The harness resolves the PATH-selected binary once, rejects a version/protocol mismatch, clears inherited `HERDR_*` variables, and injects that exact canonical path as `HERDR_BIN_PATH` for direct Tabby invocations. Review each sanitized transcript and the recorded coverage in [`docs/evidence/issue-94-herdr-contract-matrix.md`](evidence/issue-94-herdr-contract-matrix.md) before tagging.

After publishing a tag, run `python3 scripts/herdr_release_harness.py` against the real repository and release assets, passing the same `--expected-herdr-version` and `--expected-herdr-protocol` pair. The first completed native release proof and its coverage limits are recorded in [`docs/evidence/issue-79-herdr-native-release.md`](evidence/issue-79-herdr-native-release.md).

Review `.github/workflows/release.yml` and the generated Homebrew formula output before pushing the first release tag.
