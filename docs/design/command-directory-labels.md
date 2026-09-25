# Directory context for Significant Command labels

Approved scope: [spec #98](https://github.com/yersonargotev/tabby/issues/98), delivered through [ticket #99](https://github.com/yersonargotev/tabby/issues/99).

The original opt-in `command_and_directory` presentation combines the Significant Command with the existing Working Directory Suffix or directory alias, for example `codex · tabby`. As of issue #103, the built-in default is `directory_and_command`, yielding `tabby > codex`. An explicit `command_only` retains the application-only presentation. Both contextual modes accept an optional separator; their omitted-separator defaults are ` · ` and ` > ` respectively.

The mode belongs to Label Policy and follows existing configuration, profile selection, and reload semantics. Directory context uses the existing effective working directory; it introduces no filesystem discovery, sibling-tab disambiguation, or runtime ownership changes. A composed label remains a Significant Command candidate.

Contextual abbreviation preserves the command start and directory end when both fit within scalar and optional display-cell bounds. It retains complete graphemes and falls back to the bounded first component in the selected order when both portions and the separator cannot fit. The explicit `command_only` mode retains its existing truncation behavior.

Validation follows the existing configuration-to-candidate seam, runtime policy reload coverage, and an isolated native lifecycle scenario. Manual locks, Focus Quiet Window, and focused-tab-only updates remain authoritative.
