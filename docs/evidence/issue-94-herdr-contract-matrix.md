# Issue 94 Herdr contract matrix evidence

Date: 2026-08-19

Host contract: Apple Silicon macOS

Tabby implementation commit: `33e3bac61c462bb28ba1d158c46198bab8ef6522`

Result: passed

## Tested binaries

| Herdr version | Protocol | Binary source | Result |
| --- | ---: | --- | --- |
| 0.8.0 | 19 | Existing Homebrew installation at `/opt/homebrew/bin/herdr` | Passed |
| 0.8.2 | 20 | Official Apple Silicon binary from the [Herdr v0.8.2 release](https://github.com/herdrdev/herdr/releases/tag/v0.8.2), SHA-256 `a5d4f4d504d8b309c91f811050559300faba31258425f53c50852fc96f6ae574` | Passed |

The 0.8.2 binary ran from an isolated temporary directory. The operator's installed 0.8.0 binary was not replaced or modified. These results are the mandatory real-runtime evidence matrix for the JSON socket contract Tabby consumes; they are not a permanent version/protocol runtime allowlist.

## Reproduction

Each binary must be selected through `PATH`; the harness resolves it to a canonical path, injects that path as `HERDR_BIN_PATH` for direct Tabby invocations, and uses the explicit expectations to reject an accidental binary mismatch:

```sh
PATH="/path/to/herdr-0.8.0/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  python3 scripts/herdr_lifecycle_harness.py \
    --expected-herdr-version 0.8.0 \
    --expected-herdr-protocol 19 \
    --output /tmp/tabby-herdr-0.8.0-19.jsonl

PATH="/path/to/herdr-0.8.2/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
  python3 scripts/herdr_lifecycle_harness.py \
    --expected-herdr-version 0.8.2 \
    --expected-herdr-protocol 20 \
    --output /tmp/tabby-herdr-0.8.2-20.jsonl
```

Both sanitized schema-version 1 transcripts contained 106 records and 25 passing assertions. Neither run contained a failed assertion. The one nonzero command in each transcript was the deliberate invalid configuration reload used to prove that the active policy remains intact.

## Observed lifecycle coverage

The two runs exercised the same behavior matrix against the default and named Herdr sessions:

| Contract | Recorded observation |
| --- | --- |
| Startup Gate | The registered `start` action reached one Ready owner only after the selected Herdr version, protocol, and socket matched the evidence guard. |
| Concurrent hook ingress | Sixteen concurrent startup, creation, manual, and focus signals per session returned through the Ready owner. |
| Focus Quiet Window and Continuous Focused-Tab Freshness | No evaluation advanced during the Focus Quiet Window; focused labels changed on the delayed and periodic evaluations. |
| Significant Command and Working Directory Suffix | A real focused tab changed to `nvim` while the command ran and returned to its Working Directory Suffix afterward. |
| Manually Locked Tab | `manual-contract` became a persisted Manually Locked Tab and blocked a periodic automatic label update. |
| Session isolation and policy reload | Default and named sessions retained separate state for equal tab ids; a valid named policy reload applied, while an invalid reload was rejected without replacing it. |
| Client detach | A real tmux-hosted Herdr client detached and the same Ready owner completed another periodic evaluation. |
| Crash recovery | Killing the named owner released its lease; the next creation signal started a different owner and returned it to Ready. |
| Registered binary handoff | A Homebrew-shaped binary became the sole Ready owner, then control returned to the plugin-root binary without overlapping owners. |
| Session stop and restore | The default owner stopped with the Herdr server; restore created a different owner and retained the label and Manually Locked Tab state. |

The harness removed only its validated temporary roots after stopping both isolated servers. At the recorded implementation commit, deterministic runtime-status tests complemented this live proof by rejecting older, crossed, mismatched, and unknown version/protocol fixtures, including protocols 18 and 21. ADR 0014 subsequently replaced that pair allowlist with fail-closed validation of the required JSON schema subset and read-only live probes. The recorded binaries, transcript counts, assertions, checksum, and observed lifecycle outcomes remain historical evidence; a compatible later protocol may now pass the runtime gate without changing Tabby.
