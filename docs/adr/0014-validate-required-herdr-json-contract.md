# Validate the required Herdr JSON contract

Status: Accepted.

## Context

ADR 0010 established Herdr 0.8.0/protocol 19 as Tabby's minimum runtime contract. The implementation combined those sanity baselines with an exact version/protocol Startup Gate.

Herdr 0.8.2 reports protocol 20 because its incompatible attached-client binary wire format gained graphics, pixel-mouse, and terminal-bell messages. Tabby does not speak that wire format; it uses the separate newline-delimited JSON socket API. The tagged JSON interface still exposes the methods and fields Tabby consumes, so an exact pair allowlist couples Tabby to an adjacent contract. Accepting every later protocol based only on numeric or semantic-version ordering would instead fail open across unverified JSON contracts.

## Decision

One required-Herdr-contract module owns Herdr compatibility validation within Session Runtime readiness. Before the Session Runtime becomes Ready, it must:

- confirm a running server at the exact selected session socket;
- require Herdr 0.8.0 and protocol 19 as minimum sanity baselines;
- invoke the release-matched `HERDR_BIN_PATH api schema --json` and require compatible request and response shapes for `session.snapshot`, `pane.process_info`, and `tab.rename`;
- run read-only live probes for `session.snapshot` and, when a focused pane exists, `pane.process_info` against that socket; and
- accept additive fields while rejecting malformed output, missing or incompatible requirements, and contradictory status with an actionable diagnostic; transient live-probe transport failures remain recoverable through the Startup Gate.

`tab.rename` is validated from the schema rather than invoked as a startup probe because it mutates user-visible state. `HERDR_BIN_PATH` is authoritative for schema inspection; an unrelated `herdr` executable from `PATH` cannot satisfy the gate. The manifest retains `min_herdr_version = "0.8.0"` as the install-time lower bound, while runtime validation remains authoritative.

The lifecycle and release harnesses retain explicit expected version/protocol inputs as evidence guards. Herdr `0.8.0 / protocol 19` and `0.8.2 / protocol 20` remain the mandatory real-runtime matrix, but they are evidence rather than a permanent runtime allowlist. A later protocol may start without a Tabby code change only when every minimum, schema, socket, and live-probe requirement validates.

## Consequences

Herdr 0.8.2 can run Tabby's Session Runtime without treating its binary protocol bump as a JSON incompatibility. Future unrelated wire-protocol bumps no longer require routine allowlist edits, while missing methods, fields, shapes, and behavior probes still fail closed.

Schema validation cannot prove every semantic behavior behind an unchanged shape. Maintainers therefore continue to run the explicit real-runtime evidence matrix before release and add evidence for materially new Herdr contracts. A deterministic schema, response, or protocol contradiction leaves `tabby start` Faulted rather than guessing compatibility. A live-probe transport failure releases the startup attempt without persisting a terminal fault so the next lifecycle or manual hook can retry.
