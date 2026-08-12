# Install verified releases through Herdr

The canonical root manifest declares one build command that prepares `.herdr/bin/tabby` before Herdr registers a GitHub-managed checkout. The installer derives the release tag from the manifest version, supports Apple Silicon macOS by selecting the matching cargo-dist archive, requires its published SHA-256 sidecar to be well formed and correct, extracts only the expected regular-file entry, and replaces the canonical executable atomically.

An HTTP 404 for the platform archive is the only condition that permits a source build, and only when the documented stable Rust toolchain is available. Missing checksums, network failures, unsupported platforms or architectures, malformed archives, checksum mismatches, and filesystem failures abort installation without falling back or registering the plugin. The normal release path requires Python 3.9+ and network access to GitHub but does not require Rust.

Homebrew remains a supported alternative distribution adapter with identical product semantics. This decision completes ADR 0011's canonical plugin-root contract and supersedes ADR 0005's Homebrew-only release installation decision without changing Session Runtime behavior or persistence locations.
