# Directory context for Significant Command labels

Approved scope: [spec #98](https://github.com/yersonargotev/tabby/issues/98), delivered through [ticket #99](https://github.com/yersonargotev/tabby/issues/99).

The opt-in `command_and_directory` presentation combines the Significant Command with the existing Working Directory Suffix or directory alias, for example `codex · tabby`. The default `command_only` presentation remains unchanged.

The mode belongs to Label Policy and follows existing configuration, profile selection, and reload semantics. Directory context uses the existing effective working directory; it introduces no filesystem discovery, sibling-tab disambiguation, or runtime ownership changes. A composed label remains a Significant Command candidate.

Contextual abbreviation preserves the command start and directory end when both fit within scalar and optional display-cell bounds. It retains complete graphemes and falls back to a bounded command when both portions and the separator cannot fit. The legacy mode retains its existing truncation behavior.

Validation follows the existing configuration-to-candidate seam, runtime policy reload coverage, and an isolated native lifecycle scenario. Manual locks, Focus Quiet Window, and focused-tab-only updates remain authoritative.
