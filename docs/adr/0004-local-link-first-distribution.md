# Start with local plugin linking before release packaging

The first usable version will be installed locally with `cargo build` and `herdr plugin link .` rather than through a remote installer. Herdr plugins run as normal unsandboxed user code, so local linking keeps the trust and update story explicit while the plugin behavior is still being proven.

## Consequences

Release and install packaging remains pending and important. Before recommending broader installation, the project should add reproducible release builds, macOS binaries first, checksums, and an auditable install script. The plugin should not auto-update silently.
