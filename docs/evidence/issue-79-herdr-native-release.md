# Issue 79 Herdr-native release evidence

Date: 2026-08-11 (America/Bogota), 2026-08-12 UTC
Host contract: Apple Silicon macOS, Herdr 0.8.0, protocol 19
Tested release: `v0.1.13` at `69b3477acf2032a3a542b6614be0fae6f96f4082`
Result: passed

## Published release

The [`v0.1.13` GitHub Release](https://github.com/yersonargotev/tabby/releases/tag/v0.1.13) was created by [release workflow 31560564296](https://github.com/yersonargotev/tabby/actions/runs/31560564296), which completed successfully. The Cargo package, canonical manifest, Homebrew manifest, and tag all declared `0.1.13`.

The release published the exact native-installer inputs:

- `tabby-aarch64-apple-darwin.tar.xz`
- `tabby-aarch64-apple-darwin.tar.xz.sha256`

The archive digest was `00fb027a17f7251413a22e97abd8298c7967a9b5307f9306305c8e52ff10b30c`. Downloading both assets and running `python3 scripts/check-release-contract.py --artifact-dir <download>` passed. The generated Homebrew formula also references `v0.1.13` and that digest.

## Isolation and reproducibility

`scripts/herdr_release_harness.py` created one temporary root under `/tmp`, redirected `HOME`, every XDG root, `TMPDIR`, and `HERDR_CONFIG_PATH` beneath it, and removed inherited `HERDR_*` variables. Its live `PATH` included Herdr, Python, tmux, and system tools but excluded `cargo` and `rustc`, making a successful install evidence that the published prebuilt path was used. Cleanup stopped the isolated server and removed only the validated temporary root.

The harness supports `--plan --root <path>` for a non-mutating inspection of its environment and command topology. The successful schema-versioned, sanitized transcript is checked in as [`issue-79-herdr-native-release.jsonl`](issue-79-herdr-native-release.jsonl); repository and sandbox paths are replaced with `<repo>` and `<sandbox>`.

Run the proof again with:

```sh
python3 scripts/herdr_release_harness.py
```

## Real release observations

- `herdr plugin install yersonargotev/tabby --ref v0.1.13 --yes` resolved commit `69b3477acf2032a3a542b6614be0fae6f96f4082`, ran the declared Python build command, and registered plugin id `yersonargotev.tabby`, the GitHub-managed production root, one startup hook, three events, four actions, and `.herdr/bin/tabby` commands. Pinning the release tag makes future reruns independent of later `main` changes.
- The explicit `start` action produced a Ready Session Runtime on the registered `0.1.13` executable with no status warnings.
- The explicit `refresh` action renamed an eligible focused tab to the `manual-refresh-target` Working Directory Basename. A later `release-manual-lock` label became a Manually Locked Tab and survived periodic freshness.
- Reinstalling the pinned GitHub release rotated the registered executable instance. During explicit `start`, 10 ms sampling first observed prior PID 32449 exit at +261 ms and then replacement PID 32534 Ready at +281 ms; no sample observed overlapping owners. The corresponding launch ids also changed.
- Two consecutive Runtime Status reads reported `Warnings: none`, retained the same owner, and did not change Session-Scoped Tab State.
- After a real tmux-hosted Herdr client detached, the same Ready owner completed another periodic evaluation.
- Session Stop ended that owner. Session Restore created PID 32591 / launch `32590-18caf1ecb73c4050-0` and retained the manual label and lock.
- `herdr plugin uninstall yersonargotev.tabby` removed the registration. Session-Scoped Tab State remained unchanged under isolated Herdr state and outside the managed source root.

## Release-path defect found and corrected

The first attempted tag, `v0.1.12`, exposed a release-only defect: Herdr reused the managed plugin root during reinstall, while Tabby treated canonical path alone as executable identity. Explicit `start` therefore kept the previous owner instead of performing Cooperative Runtime Handoff.

The corrected installer keeps the manifest command stable at `.herdr/bin/tabby` but atomically points it to a new private executable instance after every successful install. A regression test proves that two identical installs rotate canonical identity, and the `v0.1.13` live run above proves the resulting handoff. The pending `v0.1.12` Homebrew publication was canceled so it cannot replace the corrected `v0.1.13` formula; its tag and release remain as defect traceability.

## Coverage limits

This proof covers direct repository installation, not marketplace search ranking or UI discoverability. It covers Apple Silicon macOS and Herdr 0.8.0 only. Deterministic unit and multiprocess tests remain the evidence for malformed downloads, checksum failures, unsupported platforms, protocol contradictions, and state corruption; the live release run deliberately does not inject those failures.
