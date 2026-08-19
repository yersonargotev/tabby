# Validate vetted Herdr contract pairs

Status: Accepted.

## Context

ADR 0010 established Herdr 0.8.0/protocol 19 as Tabby's minimum runtime contract. The implementation combined that minimum-version statement with an exact `protocol == 19` Startup Gate.

Herdr 0.8.2 reports protocol 20 because its incompatible attached-client wire format gained graphics, pixel-mouse, and terminal-bell messages. The tagged local JSON socket interface still exposes the methods and response fields Tabby consumes, but the exact protocol-19 gate prevents the Session Runtime from starting. Accepting every later protocol based on semantic version ordering would fail open across unreviewed contracts.

## Decision

Tabby will support only explicit Herdr version/protocol pairs whose local JSON socket subset and Session Runtime lifecycle have been verified. The initial matrix is:

- Herdr 0.8.0 / protocol 19;
- Herdr 0.8.2 / protocol 20.

One compatibility interface in the Session Runtime owns this matrix and the failure diagnostic. The manifest retains `min_herdr_version = "0.8.0"` as the install-time lower bound; runtime readiness is authoritative and rejects mismatched or unknown pairs even when their semantic version is newer.

The lifecycle and release harnesses accept explicit expected version/protocol inputs. Adding a pair requires reviewing the tagged local JSON socket methods used by Tabby and passing the complete isolated lifecycle harness against the official binary. A protocol bump is not accepted solely because existing DTOs still deserialize.

## Consequences

Herdr 0.8.2 can run Tabby's existing Session Runtime without weakening fail-closed protocol handling. Diagnostics name both the observed contract and every supported pair. Future Herdr releases require a small matrix update plus real compatibility evidence, so support is deliberate rather than accidental.

The manifest cannot express an upper bound or a version/protocol matrix. Installation may therefore succeed on an unverified Herdr version, but `tabby start` will remain Faulted with an actionable contract diagnostic until that pair is vetted.
