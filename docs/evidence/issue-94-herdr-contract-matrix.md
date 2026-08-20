# Issue 94 Herdr contract matrix evidence

Date: 2026-08-20

Host contract: Apple Silicon macOS

Tabby source under test:

- base commit: `eb834a94df49cb19e2b2029e2bd11bd714304df6`;
- `src/herdr_contract.rs` SHA-256: `5d18347b7f3f87e8f887dddf1185daa7f8decd8e9d7a0eb3b3edd6f9302ce45f`.

The base commit plus source-file fingerprint identifies the exact implementation tested independently of the later documentation commit.

Result: passed

## Tested binaries

| Herdr version | Protocol | Binary source | Result |
| --- | ---: | --- | --- |
| 0.8.0 | 19 | Official Apple Silicon binary from the [Herdr v0.8.0 release](https://github.com/herdrdev/herdr/releases/tag/v0.8.0), SHA-256 `d53a9f93fccfdfcc55632927bf51002f5add0aa7990bcdf508ffbd84ac658178` | Passed |
| 0.8.2 | 20 | Official Apple Silicon binary from the [Herdr v0.8.2 release](https://github.com/herdrdev/herdr/releases/tag/v0.8.2), SHA-256 `a5d4f4d504d8b309c91f811050559300faba31258425f53c50852fc96f6ae574` | Passed |

Both official binaries ran from isolated temporary directories. The operator's installed Herdr 0.8.2 binary was not replaced or modified. These results are the mandatory real-runtime evidence matrix for the JSON socket contract Tabby consumes; they are not a permanent version/protocol runtime allowlist.

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

The harness removed only its validated temporary roots after stopping both isolated servers. On the fingerprinted source under test, both runs exercised the dynamic gate itself: the host-selected absolute `HERDR_BIN_PATH`, minimum release and protocol, required JSON schema subset, exact socket identity, and read-only live probes all validated before each Session Runtime became Ready. Deterministic contract tests complement this live proof by rejecting older or contradictory status, malformed schema output, incompatible request and response shapes, unsupported required envelope fields, contradictory discriminators, failed probes, relative binary paths, and ambiguous response unions while accepting a compatible protocol 21 fixture. The recorded binaries, transcript counts, assertions, checksums, and observed lifecycle outcomes prove the required contract across protocols 19 and 20 without making that pair a permanent runtime allowlist.
