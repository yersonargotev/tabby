# Herdr Tab Auto-Renamer

This context describes a Herdr plugin that keeps tab labels useful by deriving them from the focused tab's meaningful foreground activity, falling back to that tab's focused pane working directory name. Tabby prioritizes mouse tab navigation over label freshness; automatic labels may lag rather than disturbing clicks in the tab bar.

## Language

**Tab Label Candidate**:
A short, user-facing label the plugin may apply to a Herdr tab after inspecting the tab's focused pane. It is either a Significant Command label or a Working Directory Basename.
_Avoid_: title, name, tab title

**Significant Command**:
A foreground app or command that is stable and meaningful enough to represent the tab, such as `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, or `go test`. Shells, opaque wrappers, and transient foreground processes are not Significant Commands.
_Avoid_: foreground process, process name, app

**Working Directory Basename**:
The final path component of the focused pane's current working directory, used only when there is no Significant Command candidate. For example, `/Users/me/dev/dots` becomes `dots`.
_Avoid_: full path, cwd label

**Manually Locked Tab**:
A Herdr tab whose user-facing label changed outside the plugin's own rename operation after Tabby has established a plugin-label baseline, so the plugin must stop auto-renaming it. Manual locks persist across plugin runs until an explicit unlock mechanism removes them.
Herdr may reuse a `tab_id` after tab or workspace churn. When a tab's visible label exactly matches its reported tab number, Tabby treats that default numeric label as a fresh tab lifecycle, discards persisted lock/baseline state for the reused ID, and resumes automatic naming. Other numeric labels remain eligible for manual locking.
_Avoid_: manual rename, ignored tab, disabled tab

**Unlock Action**:
A user-invoked plugin action that removes one or more Manually Locked Tabs and their associated plugin-label baselines from the persisted lock store so automatic naming can resume without immediately recreating the same lock. Baselines for tabs that were not locked remain intact. The Hybrid Session Refresher observes these persisted changes before its next refresh outside the Focus Quiet Window. The expected actions are unlock focused tab and unlock all tabs.
_Avoid_: reset, auto-unlock

**Stable Label Candidate**:
A Tab Label Candidate considered safe to apply with `tab.rename` to the currently focused unlocked tab. In the One-Shot Refresh design, the short stabilization delay happens before inspection and Tabby applies at most one candidate from the focused tab before exiting.
_Avoid_: immediate label, debounced title

**Pending Rename**:
A Stable Label Candidate held by the Hybrid Session Refresher for possible later application. Current hybrid behavior does not discover new Pending Renames during the Focus Quiet Window because the window performs no Herdr API calls; after quiet, Tabby revalidates by reading the focused tab normally.
_Avoid_: queued title, delayed rename, cached label

**Inactive Tab**:
A Herdr tab that Herdr does not currently report as focused. The Hybrid Session Refresher does not inspect processes or apply renames to Inactive Tabs; their last visible label is preserved until a later refresh sees them focused and outside the Focus Quiet Window.
_Avoid_: background tab, hidden tab

**Navigation Stability**:
The user-facing guarantee that clicking or otherwise navigating between Herdr tabs must not be disrupted by Tabby's automatic label updates. Navigation Stability is more important than immediate label freshness.
_Avoid_: click workaround, UI quirk, placebo fix

**Focus Quiet Window**:
A 1000 ms interval after a Herdr focus trigger during which the Hybrid Session Refresher must not call any Herdr API, including `tab.list`, `pane.list`, `pane.process_info`, or `tab.rename`. Every delivered focus trigger resets the window, even when it is stale, replayed, or apparently redundant; after the window, Tabby evaluates the current focus rather than trusting the trigger payload.
_Avoid_: debounce, delay, cooldown

**Refresh Trigger**:
A discrete Herdr navigation or lifecycle event, or an explicit user action, that permits Tabby to evaluate whether the focused tab label should change. Accepted Refresh Triggers are tab focus, workspace focus, tab creation, workspace creation, and manual refresh.
_Avoid_: polling signal, output event, every tick

**Zero Recurring Idle Work**:
The end-to-end resource guarantee that an idle Tabby installation performs no periodic work attributable to Tabby, including incremental work inside Herdr caused by Tabby's subscriptions. A compatibility mode that permits recurring upstream subscription work does not satisfy this guarantee and must be explicitly accepted.
_Avoid_: Tabby-only idle, approximately idle, passive client

**Idle-Compliant Subscription**:
A Herdr event subscription that performs no recurring work while idle and wakes only for a matching event, connection closure, or server shutdown. Herdr must declare this capability explicitly; its version alone is not evidence of compliance.
_Avoid_: polling subscription, version-compatible subscription, idle-ish stream

**Session Startup Capability**:
An explicit Herdr guarantee that Tabby receives a session-scoped startup request for every newly started or restored Herdr Session, without enumerating sessions or polling for their existence. Herdr 0.7.3 does not provide this guarantee; its creation events and explicit start actions provide only opted-in best-effort compatibility for sessions that become active after restoration.
_Avoid_: inferred startup, global session scan, restored-session guarantee

**Subscriber Startup Gate**:
The single session-scoped arbitration boundary through which every automatic or explicit startup request must pass. It establishes at most one Tabby subscriber for the Herdr Session and never treats the request source itself as proof that no subscriber already owns the session.
_Avoid_: startup hook ownership, direct subscriber spawn, global supervisor

**Subscriber Lease**:
An exclusive session-scoped ownership claim held continuously by the live Tabby subscriber and released automatically when that process ends. The lease, rather than a PID or metadata timestamp, is the authority that prevents two subscribers from owning the same Herdr Session; contradictory metadata while the lease is held is an unknown-owner condition that fails closed.
_Avoid_: heartbeat, PID lock, startup lock

**Subscriber Runtime Identity**:
The diagnostic identity associated with a Subscriber Lease: canonical Herdr Session identity; PID, operating-system process-start time, boot-session identity, and random launch identifier; plus the lossless canonical executable path and SHA-256 of its launch-time bytes. It detects stale records, PID reuse, system restart, and binary replacement but does not replace the lease as proof of liveness or authorize PID-only termination.
_Avoid_: PID identity, metadata liveness, process-name match

**Subscriber Readiness**:
The point at which a newly created subscriber has accepted its Subscriber Lease, validated the Herdr Session, completed subscription negotiation, and can safely be published as that session's live owner. A spawned process that has not confirmed Subscriber Readiness is not a live subscriber and must not leave authoritative ownership metadata behind.
_Avoid_: process spawned, PID recorded, optimistic startup

**Cooperative Subscriber Handoff**:
An explicit replacement in which the current subscriber validates a session-scoped control request, cancels unfinished evaluation, releases its Herdr connections and Subscriber Lease, and allows Subscriber Startup Gate to start the requested binary only after confirming there is no remaining owner. Ordinary lifecycle hooks may report that a replacement is needed but never initiate one or forcibly terminate a PID.
_Avoid_: automatic upgrade, kill-and-restart, overlapping replacement

**Recoverable Subscriber Failure**:
A loss of the event or RPC transport that invalidates the current evaluation and permits a bounded attempt to restore both connections for the same Herdr Session. It is distinct from an application-level failure involving one tab or pane.
_Avoid_: retryable rename, any error, permanent disconnect

**Transient Evaluation Failure**:
An application-level failure confined to the current One-Shot Refresh, such as a tab, pane, or rename target disappearing while the underlying Herdr transports remain healthy. It ends that attempt and returns the subscriber to Idle without reconnecting or creating a recovery timer.
_Avoid_: subscriber failure, fatal RPC, retry loop

**Terminal Subscriber Fault**:
A deterministic contradiction in session identity, ownership, protocol, or required capability that cannot become valid by repeating the same connection attempt. The subscriber fails closed and requires an explicit corrected condition or action rather than automatic retry.
_Avoid_: transient disconnect, recoverable error, silent stop

**Recovery Episode**:
One bounded attempt to restore a subscriber after a Recoverable Subscriber Failure. It may reconnect and renegotiate the Herdr transports but may not inspect tabs or rename them; successful resubscription starts a fresh One-Shot Refresh, while repeated short-lived connections consume the same recovery budget.
_Avoid_: reconnect loop, background retry, recovery polling

**Recovery Paused**:
A degraded, timer-free subscriber state entered when a Recovery Episode exhausts its bounded work without proving that the Herdr Session is gone. It retains the Subscriber Lease and may wake only for an explicit recovery request or an operating-system notification concerning the session socket.
_Avoid_: retry exhausted exit, periodic reconnect, healthy idle

**Session Definitively Gone**:
A terminal Herdr Session condition proven either by an authenticated session-close notification or by transport loss together with direct observation that the canonical session socket was removed. EOF alone, an unresponsive server, or an existing but unusable socket is insufficient proof and must not be converted into a liveness timer.
_Avoid_: disconnected session, retry exhausted, socket timeout

**Subscriber Conformance Mode**:
The negotiated resource and lifecycle contract under which a subscriber runs. Strict mode requires explicit Idle-Compliant Subscription, Session Startup Capability, and a compatible event contract; Herdr 0.7.3 compatibility requires persistent opt-in and must always report both recurring upstream idle work and incomplete restored-session coverage.
_Avoid_: automatic fallback, version-based compliance, transparent compatibility

**One-Shot Refresh**:
A bounded automatic label refresh attempt started by a Refresh Trigger. After the Focus Quiet Window, it takes at most three samples at a 500 ms cadence and requires two consecutive equal candidates before revalidation. A newer focus trigger invalidates the attempt; otherwise, a successful rename must complete within 2.5 seconds of the focus change that produced the last trigger when Herdr remains responsive. The attempt applies at most one automatic label update and ends without a rename when the candidate does not stabilize within its sample bound.
_Avoid_: daemon loop, background polling, continuous refresh

**Focused Pane**:
The pane within the focused tab that Herdr reports as focused. If no pane in the focused tab is reported as focused, the plugin may use the first listed pane only for Working Directory Basename fallback.
_Avoid_: active pane, selected pane

**Label Policy**:
The rules used to turn process and cwd data into a Tab Label Candidate, including Significant Command allowlists, ignored shells/wrappers, and stability timings. Version 1 uses tested built-in defaults; user configuration is a later slice.
_Avoid_: config, preferences, ruleset

**Process Inspector**:
The boundary that asks Herdr for foreground process details for a selected pane. If process inspection fails or returns no useful Significant Command, the plugin falls back to Working Directory Basename rather than failing the rename loop.
_Avoid_: process_info call, ps lookup

**Herdr Session**:
A running Herdr server context identified by the socket that plugin commands use to inspect and rename tabs. Tabby's automatic behavior is scoped to one Herdr Session at a time.
_Avoid_: terminal session, shell session

**Session-Scoped Tab State**:
Persistent Manually Locked Tabs and plugin-label baselines that belong to exactly one Herdr Session. Tab identities are meaningful only within that session; the same `tab_id` in another session refers to unrelated state and must never inherit its locks or baselines.
_Avoid_: global tab state, shared lock store

**Tabby Session Daemon**:
A legacy long-running Tabby process from the superseded pre-hybrid polling design. Use Hybrid Session Refresher for current behavior.
_Avoid_: current refresh process, plugin action process

**Hybrid Session Refresher**:
A long-running Tabby process scoped to one Herdr Session that restores automatic label freshness while preserving Navigation Stability. It observes the focused tab on a low-cadence idle interval, never inspects or renames Inactive Tabs, and performs no Herdr API calls during the Focus Quiet Window.
_Avoid_: old daemon, polling daemon, background renamer
