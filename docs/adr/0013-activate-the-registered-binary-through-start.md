# Activate the registered binary through Start

Status: Accepted. Refines ADR 0010's Cooperative Runtime Handoff decision.

## Context

GitHub-managed and Homebrew manifests can register different Tabby executable paths for the same Herdr Session. Routine startup, focus, and creation hooks must remain idempotent recovery ingress, but an explicit activation after install, upgrade, reinstall, or distribution migration must make the binary selected by the registered manifest authoritative.

## Decision

The public `start` action invokes `tabby start`. That command crosses the Startup Gate and ensures its own canonical executable identity becomes the Ready Session Runtime. It signals an already Ready matching owner, starts one when absent, or requests authenticated Cooperative Runtime Handoff from a different validated owner before waiting for lease release and starting the replacement. Canonical identity resolves the stable manifest command to its private per-install executable instance, allowing reinstall/update in a reused managed root to remain distinguishable from the running owner.

Manifest startup hooks continue to invoke `tabby ensure-started`; focus and creation hooks keep their existing signal commands. These paths never request replacement. No path treats PID metadata as termination authority. Runtime Status identifies the current executable, registered command binary, and Ready owner binary, and recommends the distribution-neutral Herdr `start` action when those identities differ.

## Consequences

Explicit activation can move ownership in either direction between GitHub-managed and Homebrew binaries without overlapping owners or changing Session-Scoped Tab State. Invalid peers, identity contradictions, Starting or Faulted owners, and handoff timeouts fail closed. Routine hooks cannot unexpectedly replace a working runtime from another distribution.
