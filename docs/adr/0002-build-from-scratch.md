# Build the Herdr tab auto-renamer from scratch

The plugin will be implemented from scratch rather than forked from `lmilojevicc/herdr-tab-rename`. The existing plugin remains useful prior art for Herdr socket polling, cwd-basename fallback, and manual lock behavior, but this project needs app-first label classification, explicit anti-flapping behavior, persistent lock management, and a testable internal module structure from the beginning.
