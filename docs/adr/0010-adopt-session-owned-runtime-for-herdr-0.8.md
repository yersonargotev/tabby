# Adopt a session-owned runtime for Herdr 0.8

Status: Accepted. Supersedes ADR 0009 and replaces ADR 0006's registration-only install decision. Preserves ADR 0007's focused-tab-only rule and ADR 0001's manual-intent priority.

## Context

The prior long-running refresher restored label freshness by combining a persistent `events.subscribe` connection with five-second observation. It survives a client detach because it is detached from the invoking terminal, but it treats stream and RPC failures as generic process errors, leaves stale startup metadata, and has no guaranteed restart path when Herdr restores a session.

Herdr 0.8.0, protocol 19, provides two better lifecycle inputs: a plugin `[[startup]]` hook for newly started and restored sessions, and `[[events]] pane.focused` hooks for focus ingress. It also provides `session.snapshot` for a coherent view of workspaces, tabs, panes, and focus. A persistent `events.subscribe` connection remains an inferior ingress because the verified implementation polls upstream every 100 ms and does not remove the need for periodic foreground-process observation.

Foreground activity can change while focus remains fixed, and Herdr does not emit an event for every such change. Tabby therefore cannot provide both continuous focused-tab freshness and zero recurring idle work. The product chooses freshness with a moderate five-second cadence while retaining Navigation Stability as the stronger invariant.

Reliable detach, stop, and restore behavior also requires ownership that outlives a startup command, readiness that is stronger than observing a PID, state isolated by Herdr Session, and explicit handling for ambiguous rename outcomes.

## Decision

Tabby will support macOS with Herdr 0.8.0 and protocol 19 as its minimum runtime contract. Compatibility modes for Herdr 0.7.x and an event-subscription-based strict mode will not be retained.

One Session Runtime will own each running Herdr Session:

- It survives Client Detach, ends when Session Stop is proven, and is created again through `[[startup]]` on Session Restore.
- All startup requests cross a short-lived Startup Gate. The owner holds an exclusive Session Runtime Lease for its entire lifetime and publishes readiness only after validating Session Identity and opening its Runtime Control Endpoint. PID and metadata are diagnostic, not ownership authority.
- Plugin startup, focus, creation, and manual hooks deliver signals and never sleep, inspect panes, or rename tabs. `[[startup]]` starts the owner and requests an initial evaluation; `pane.focused` is the primary focus ingress; creation hooks recover a missing owner.
- The owner begins a bounded focused-tab evaluation every five seconds while Ready. It does not use `events.subscribe` and does not inspect or rename Inactive Tabs.
- An unexpected runtime exit is recovered by the next lifecycle, focus, creation, or manual hook. Tabby will not add an external supervisor.

Focused-tab evaluation will be coordinated inside the Session Runtime:

- A Refresh Trigger opens or resets a 1000 ms Focus Quiet Window with no Herdr inspection or rename work.
- Each triggered or periodic evaluation has a 2.5-second absolute deadline, takes no more than three samples at a 500 ms cadence, and requires two consecutive equal candidates.
- A newer trigger supersedes unfinished work. Tabby revalidates current focus, lifecycle, visible label, lock state, and candidate before applying at most one rename.
- The Herdr adapter will obtain coherent focus and pane data from `session.snapshot`; foreground process data remains a separate `pane.process_info` observation.

Persistent state will be scoped to a lossless canonical Session Identity, with a SHA-256-derived storage key and embedded identity validation. Manual locks, Automatic Label Baselines, and unresolved Automatic Rename Intents survive Session Stop and Session Restore. Samples, triggers, deadlines, and unfinished evaluations do not. An Automatic Rename Intent is persisted before mutation and reconciled from the next visible label. Invalid persisted state fails closed and requires explicit repair; stopped-session state is removed only by a Forget Session Action.

Runtime failures will distinguish application races, ambiguous transport failures, proven Session Stop, and deterministic terminal faults. A missing target ends only the current evaluation. An ambiguous RPC failure leaves the owner available for the next trigger or periodic cycle. Socket disappearance or a definitive no-listener error proves Session Stop. Protocol, identity, ownership, and state-integrity contradictions fail closed and appear in Runtime Status.

Plain `tabby install` will register the plugin and ensure the current Session Runtime. If a different installed binary already owns the session, installation uses a Cooperative Runtime Handoff through the authenticated local control endpoint; failure does not authorize PID-based termination. Runtime Status remains read-only and reports lifecycle, ownership, version, recent evaluation/failure data, periodic scheduling, and persisted-state counts.

The architecture will use deep modules with these responsibilities:

- Session Runtime owns lifecycle, timing, serialization, and effects.
- Trigger Ingress translates Herdr hooks and manual actions into runtime signals.
- Session-Scoped Tab State owns identity validation, atomic persistence, reconciliation, and explicit cleanup.
- Focused Observation hides `session.snapshot` and `pane.process_info` orchestration.
- Refresh Decision consumes triggers, observations, time, and state and produces bounded decisions without performing Herdr I/O.

## Considered Options

- Keep the prior long-running refresher and persistent subscription. This preserves its implementation shape but retains unnecessary upstream polling and does not solve restore ownership or session-scoped state.
- Use only focus hooks and one-shot refreshes. This removes recurring work but knowingly leaves labels stale when foreground activity changes without focus.
- Run a global supervisor across all Herdr Sessions. This improves crash recovery latency but adds cross-session discovery, lifecycle, and ownership complexity outside Herdr's plugin model.
- Preserve Herdr 0.7.x compatibility modes. This increases branches and testing burden for a contract that is no longer the installed baseline.

## Consequences

Tabby can state detach, stop, and restore behavior precisely and can recover restored sessions through a supported Herdr hook. Ownership and persisted intent no longer depend on PID liveness or globally keyed tab IDs. `session.snapshot` reduces split-read races, while Automatic Rename Intent makes ambiguous mutations recoverable.

Continuous foreground freshness intentionally costs one bounded evaluation cycle every five seconds per Ready Session Runtime. Runtime crashes are not recovered until another supported hook or manual action occurs. Stopped-session state may remain on disk until explicitly forgotten. Installation now has the side effect of starting or cooperatively replacing the current runtime.

This decision replaces the former startup metadata model, persistent subscription ingress, plugin manifests, shared cross-session state, duplicated refresh orchestration, install contract, status projections, and predecessor documentation.
