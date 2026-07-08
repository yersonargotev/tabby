# Use Rust for the plugin implementation

The Herdr tab auto-renamer will be implemented in Rust. Rust aligns with Herdr's ecosystem and still gives the project a native single-binary plugin suitable for a daemon, Unix-socket JSON-RPC client, persisted lock state, and deterministic tests. This is more ceremony than Go for a small daemon, but the ecosystem fit and robustness are worth the extra setup for this project.
