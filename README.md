# Tabby

Tabby is a Herdr plugin that keeps the focused tab label meaningful within each Herdr Session. One session-owned runtime survives client detach, stops with the Herdr Session, and resumes on restore. It prefers stable foreground activity such as `nvim`, `codex`, or `pnpm dev`, then falls back to the configured Working Directory Suffix.

## Quick path

Find Tabby in the [Herdr community marketplace](https://herdr.dev/plugins/) through
the `herdr-plugin` topic, then install it on Apple Silicon macOS:

```sh
herdr plugin install yersonargotev/tabby
herdr plugin action invoke start --plugin yersonargotev.tabby
```

The marketplace is an automatic community index; a listing is not a security review or endorsement by Herdr. Its install action resolves to the same `yersonargotev/tabby` source shown above. Herdr previews and runs the repository's single build command before registration. It installs the checksum-verified release binary at `.herdr/bin/tabby`; Rust is needed only when that release has no matching Apple Silicon artifact. Homebrew remains an optional alternative, not a prerequisite.

Tabby refreshes automatically through Herdr events and periodic evaluation. To test it immediately, optionally run either manual refresh command; both request the same refresh through different paths:

```sh
tabby refresh
# or
herdr plugin action invoke refresh --plugin yersonargotev.tabby
```

For the full install, verification, trust-model, uninstall, and rollback guide, see [`docs/install.md`](docs/install.md).

## What Tabby does

Tabby automatically renames Herdr tabs using this policy:

| Priority | Label source | Examples |
| --- | --- | --- |
| 1 | Significant Command | `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, `npm test`, `go test`, `cargo run` |
| 2 | Working Directory Suffix | `/Users/me/dev/tabby` becomes `tabby` by default, or `dev/tabby` with two components |

It also avoids common shell and wrapper processes such as `zsh`, `bash`, `tmux`, `env`, and `sudo`, so normal shell tabs still get useful directory labels.

## Behavior details

- Runs one lease-owned Session Runtime per Herdr Session and starts it through Herdr 0.8 lifecycle/focus hooks or any manual action.
- Uses authenticated cooperative handoff after `tabby install`; it never treats PID metadata as ownership authority.
- Suppresses all Herdr API calls during the 1000 ms Focus Quiet Window after a Refresh Trigger.
- Re-evaluates only the focused tab every 5 seconds, with at most three samples at 500 ms cadence and two equal consecutive candidates before rename.
- Leaves inactive tab labels unchanged so Tabby does not rewrite the tab bar while the user is navigating between tabs.
- Routes refresh and unlock actions through the owner so effects remain serialized.
- Persists locks, baselines, and pre-rename intent by lossless Session Identity across stop/restore.

Project vocabulary and domain rules live in [`CONTEXT.md`](CONTEXT.md).

## Commands

```text
Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install|config <path|check|reload>|unlock-focused|unlock-all|repair-state --discard|forget-session>
```

| Command | Purpose |
| --- | --- |
| `tabby status` | Read authoritative runtime, session-state, registration, and focused-tab diagnostics without changing state. |
| `tabby refresh` | Deliver a manual Refresh Trigger to the Ready Session Runtime. |
| `tabby start` | Activate the invoking registered binary, using Cooperative Runtime Handoff when another validated binary is Ready. |
| `tabby ensure-started` | Idempotently signal a Ready owner or recover an absent owner without replacing a different binary. |
| `tabby install` | Refresh registration and cooperatively ensure the installed binary owns the current Herdr Session. |
| `tabby config path` | Print the resolved `config.toml` path without creating it. |
| `tabby config check` | Parse and validate `config.toml` without changing runtime state. |
| `tabby config reload` | Ask the Ready Session Runtime to atomically replace its active Label Policy. |
| `tabby unlock-focused` | Clear the manual lock and plugin-label baseline for the focused Herdr tab so automatic naming resumes. |
| `tabby unlock-all` | Clear all persisted manual locks and their associated plugin-label baselines so automatic naming resumes. |
| `tabby repair-state --discard` | Explicitly archive invalid session-state evidence and create clean state. |
| `tabby forget-session` | Remove retained state for the selected stopped Herdr Session. |

Run `tabby status` first when labels are not updating. It reports Absent, Starting, Ready, or Faulted; lease ownership; runtime identity/version; the latest evaluation/failure and next periodic cycle; and per-session lock, baseline, and intent counts. The command is read-only.

## Label Policy configuration

`tabby config path` resolves the versioned file under Herdr's plugin configuration directory. A missing file uses the built-in defaults and is not an error. A minimal customization is:

```toml
version = 1

[labels]
max_length = 32
max_display_width = 32
cwd_components = 1

[labels.prefixes]
"lazygit" = "git: "
"pnpm dev" = "run: "

[commands]
additional_significant = ["yazi", "btop", "k9s", "docker"]
additional_ignored = ["python", "node", "sleep"]

[commands.runners]
pnpm = ["dev", "test", "lint"]
npm = ["run", "test"]
cargo = ["run", "test", "watch"]
go = ["run", "test"]

[commands.aliases]
"lazygit" = "git"
"pnpm dev" = "dev"
"cargo test" = "tests"

[directories.aliases]
"~/dev/tabby" = "tabby"
"/Users/me/work/customer-api" = "api"
```

All fields except `version` are optional. Additional commands and runner pairs extend the built-ins; command aliases change presentation only after classification. `labels.prefixes` is keyed by those classified Significant Command and runner/subcommand candidates: aliases apply first, then the candidate's prefix, then Tabby truncates the final label once. There are no prefix or icon defaults. Directory aliases replace only the Working Directory Suffix fallback, so a Significant Command still wins. Defaults are `max_length = 32`, `cwd_components = 1`, Significant Commands `nvim`, `lazygit`, `codex`, and `claude`, runner pairs `pnpm dev`, `npm test`, `go test`, and `cargo run`, plus the ignored shell/wrapper list described above. `max_length` accepts 1–128 Unicode scalars; optional `max_display_width` accepts 1–256 display cells; and `cwd_components` accepts 1–8 trailing components.

`max_display_width` uses [`unicode-width` 0.2.2](https://docs.rs/unicode-width/0.2.2/unicode_width/) with Unicode 17.0.0 tables. Its conservative non-CJK policy treats ASCII as one cell, CJK wide characters as two, fully-qualified emoji ZWJ sequences as two, and ambiguous-width characters as narrow; it preserves combining sequences. Private-use glyphs are bounded by the Unicode tables, but exact rendering depends on the user's terminal and font, so Tabby does not promise font-perfect widths.

Directory alias selectors must be absolute paths or `~/...`; `~` expands when configuration loads. Tabby compares the effective directory (`foreground_cwd`, then pane `cwd`) by exact lexical path after collapsing `.` and `..`. This comparison never reads the filesystem: paths may be nonexistent, and distinct symlink spellings remain distinct. Globs, prefix matching, case folding, canonicalization, and automatic symlink resolution are intentionally unsupported.

Unknown fields, unsupported versions, unsafe labels, unknown or contradictory prefix candidates, duplicate normalized directory selectors, contradictions, and out-of-range values are rejected with field-specific diagnostics. Run `tabby config check`, then `tabby config reload`; a rejected reload keeps the last valid policy and records the diagnostic in `tabby status`. An invalid initial file prevents the Session Runtime from becoming Ready. Runtime timing, Navigation Stability, manual locks, leases, ownership, and persistence are intentionally not configurable.

### Per-Session profiles

The global policy above is the fallback for sessions without a selector. A selected profile is instead compiled from built-in defaults and its optional profile inheritance; it does not inherit the global policy. Child scalar fields override their parent, command lists add entries, and duplicate map keys across inheritance are rejected rather than silently shadowed.

```toml
[profiles.work]
extends = "engineering"

[profiles.work.labels]
cwd_components = 2

[profiles.engineering.commands]
additional_significant = ["yazi"]

[[session_selectors]]
profile = "work"
identity = "/Users/me/.config/herdr/sessions/work/herdr.sock"

[[session_selectors]]
profile = "personal"
named_session = "personal"
```

Each selector has exactly one of `identity`, `identity_hex`, or `named_session`. `identity` is an absolute readable socket path. `identity_hex` is the lowercase, lossless byte encoding printed by `tabby status` and supports identities that cannot be represented as UTF-8. `named_session` expands from the receiving runtime's documented socket root to `sessions/<name>/herdr.sock`, and is rejected for custom socket layouts where that root cannot be derived. All forms match against Tabby's exact resolved Session Identity. Duplicate resolved selectors, unknown profiles, invalid inheritance, and cycles are errors.

`tabby config reload` is session-local: it recompiles and atomically replaces only the receiving Ready runtime's selected policy. It never reloads every session or changes locks, baselines, rename intents, leases, ownership, or Session Identity. `tabby status` reports the selected profile and `policy_source` for the active valid policy and retains both when a later reload is rejected.

## Local development

Build the local debug binary, prepare the canonical plugin-root executable, and link this checkout:

```sh
cargo build
python3 scripts/prepare-herdr-plugin.py
herdr plugin link .
```

The root [`herdr-plugin.toml`](herdr-plugin.toml) is the production-shaped manifest shared by local linking and the GitHub-managed distribution path. Every startup hook, event, and action invokes `.herdr/bin/tabby`. Because `herdr plugin link` intentionally does not run manifest build commands, rerun both `cargo build` and `python3 scripts/prepare-herdr-plugin.py` after code changes. The prepared `.herdr/` directory is local build output and is not committed.

Tabby reads plugin configuration and Session-Scoped Tab State from Herdr-provided configuration/state directories (with XDG fallbacks), never from `.herdr/` or another location inside this checkout.

## Verification

Run the focused local checks before opening a PR:

```sh
cargo fmt -- --check
git diff --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 scripts/check-herdr-manifests.py
python3 -m unittest discover -s scripts/tests
cargo build
python3 scripts/prepare-herdr-plugin.py
```

On macOS with Herdr 0.8.0 installed, run the isolated real-lifecycle harness:

```sh
python3 scripts/herdr_lifecycle_harness.py
```

The harness prepares `.herdr/bin/tabby` from the existing debug build, links the root manifest, uses temporary HOME/XDG/config roots, exercises default and named Herdr Sessions, writes a sanitized JSONL transcript under `.scratch/`, and removes its temporary sessions and state. It never falls back to the operator's Herdr configuration.

For release planning, also run:

```sh
dist plan --output-format=json > plan-dist-manifest.json
python3 scripts/check-release-contract.py --dist-manifest plan-dist-manifest.json
```

## Release notes

Tabby's release path uses `dist`/`cargo-dist` to publish an Apple Silicon macOS archive, its SHA-256 checksum, and a Homebrew formula. Herdr-managed installation derives the release version from the canonical manifest, verifies the published checksum, and atomically prepares `.herdr/bin/tabby` before registration. The Homebrew package remains an alternative adapter whose product semantics are validated against the canonical root manifest.

Release setup and tagging details live in [`docs/release.md`](docs/release.md). The development and release manifests are kept aligned by [`scripts/check-herdr-manifests.py`](scripts/check-herdr-manifests.py).

## Documentation map

| File | Use |
| --- | --- |
| [`docs/install.md`](docs/install.md) | User install, verification, trust model, uninstall, and rollback. |
| [`docs/release.md`](docs/release.md) | Maintainer release process and required GitHub secret. |
| [`docs/design/architecture.md`](docs/design/architecture.md) | Architecture and module responsibilities. |
| [`docs/evidence/issue-71-herdr-0.8-lifecycle.md`](docs/evidence/issue-71-herdr-0.8-lifecycle.md) | Recorded real-Herdr 0.8 lifecycle evidence and coverage limits. |
| [`docs/evidence/issue-79-herdr-native-release.md`](docs/evidence/issue-79-herdr-native-release.md) | Recorded native release install, activation, lifecycle, and cleanup evidence. |
| [`docs/evidence/issue-80-herdr-marketplace.md`](docs/evidence/issue-80-herdr-marketplace.md) | Recorded public marketplace discovery and final isolated install smoke evidence. |
| [`docs/adr/`](docs/adr/) | Accepted architecture decisions. |
| [`docs/herdr-tab-title-research.md`](docs/herdr-tab-title-research.md) | Historical research that preceded the implemented Session Runtime. |
