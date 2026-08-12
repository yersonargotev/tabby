# Herdr Tab Auto-Renamer

Tabby keeps the focused tab label meaningful within each Herdr Session. It prefers stable foreground activity, falls back to the focused pane's working directory name, preserves Navigation Stability and valid manual intent, continues across Client Detach, ends with Session Stop, and resumes through Session Restore.

## Language

**Tab Label Candidate**:
A short, user-facing label the plugin may apply to a Herdr tab after inspecting the tab's focused pane. It is either a Significant Command label or a Working Directory Suffix.
_Avoid_: title, name, tab title

**Significant Command**:
A foreground app or command that is stable and meaningful enough to represent the tab, such as `nvim`, `lazygit`, `codex`, `claude`, `pnpm dev`, or `go test`. Shells, opaque wrappers, and transient foreground processes are not Significant Commands.
_Avoid_: foreground process, process name, app

**Working Directory Suffix**:
The configured number of trailing path components from the focused pane's current working directory, used only when there is no Significant Command candidate. It defaults to one component, so `/Users/me/dev/dots` becomes `dots`; with two components it becomes `dev/dots`.
_Avoid_: Working Directory Basename, full path, cwd label

**Manually Locked Tab**:
A Herdr tab whose user-facing label changed outside the plugin's own rename operation after Tabby has established a plugin-label baseline, so the plugin must stop auto-renaming it. Manual locks persist across plugin runs until an explicit unlock mechanism removes them.
Herdr may reuse a `tab_id` after tab or workspace churn. When a tab's visible label exactly matches its reported tab number, Tabby treats that default numeric label as a fresh tab lifecycle, discards persisted lock/baseline state for the reused ID, and resumes automatic naming. Other numeric labels remain eligible for manual locking.
_Avoid_: manual rename, ignored tab, disabled tab

**Automatic Label Baseline**:
The last tab label that Tabby confirmed as its own successful result. It survives Session Restore for the same Herdr Session so Tabby can distinguish its work from later manual intent.
_Avoid_: last seen label, cached label, current candidate

**Unlock Action**:
A user-invoked plugin action that removes one or more Manually Locked Tabs and their associated plugin-label baselines from the persisted lock store so automatic naming can resume without immediately recreating the same lock. Baselines for tabs that were not locked remain intact. The Session Runtime observes these changes before its next refresh outside the Focus Quiet Window. The expected actions are unlock focused tab and unlock all tabs.
_Avoid_: reset, auto-unlock

**Stable Label Candidate**:
A Tab Label Candidate considered safe to apply with `tab.rename` to the currently focused unlocked tab. Tabby requires two consecutive equal samples, revalidates the focused tab, and applies at most one candidate during a bounded evaluation.
_Avoid_: immediate label, debounced title

**Automatic Rename Intent**:
A crash-safe record written immediately before `tab.rename` that identifies the session, tab, previous visible label, and intended Automatic Label Baseline. On the next Session Runtime start, the visible target confirms the baseline, the unchanged source discards the intent, a fresh default numeric label discards reused-tab state, and any other label is preserved as manual intent.
_Avoid_: pending candidate, queued title, successful rename

**Inactive Tab**:
A Herdr tab that Herdr does not currently report as focused. The Session Runtime does not inspect processes or apply renames to Inactive Tabs; their last visible label is preserved until a later refresh sees them focused and outside the Focus Quiet Window.
_Avoid_: background tab, hidden tab

**Navigation Stability**:
The user-facing guarantee that clicking or otherwise navigating between Herdr tabs must not be disrupted by Tabby's automatic label updates. Navigation Stability is more important than immediate label freshness.
_Avoid_: click workaround, UI quirk, placebo fix

**Focus Quiet Window**:
A 1000 ms interval after a Refresh Trigger during which the Session Runtime does not inspect or rename through Herdr. Every delivered focus trigger resets the window; afterward Tabby evaluates current focus rather than trusting the trigger payload.
_Avoid_: debounce, delay, cooldown

**Refresh Trigger**:
A discrete Herdr navigation, creation, startup, or explicit user action that permits Tabby to schedule evaluation of the currently focused tab. A manual action delivers its trigger immediately but does not bypass an active Focus Quiet Window. A newer trigger supersedes unfinished evaluation work. Periodic observation maintains freshness but is not a Refresh Trigger.
_Avoid_: polling signal, output event, every tick

**Continuous Focused-Tab Freshness**:
The product guarantee that a Ready Session Runtime begins a bounded re-evaluation of the focused tab every five seconds even when no navigation event occurs. It applies during Client Detach. After an unexpected runtime exit, the next lifecycle, focus, creation, or manual hook crosses the Startup Gate and restores the guarantee; Tabby does not run an external supervisor.
_Avoid_: instant freshness, focus-only freshness, background-tab freshness

**Client Detach**:
A client disconnect from a running Herdr Session that leaves its server, panes, and Session Runtime alive.
_Avoid_: Session Stop, shutdown, exit

**Session Stop**:
The end of a running Herdr server and its pane processes. Tabby considers it proven when the canonical socket disappears or a connection reports that no server is listening, such as `ENOENT` or `ECONNREFUSED`; a timeout or ambiguous RPC failure is not sufficient. The Session Runtime ends and releases its lease, while saved session structure and Session-Scoped Tab State may later be used by Session Restore.
_Avoid_: Client Detach, temporary disconnect

**Session Restore**:
The creation of a new running Herdr server from saved session structure. It creates a new Session Runtime rather than continuing the process that ended at Session Stop.
_Avoid_: reattach, process continuation, Client Detach

**Session Runtime**:
The single live Tabby owner associated with one running Herdr Session. It holds the Session Runtime Lease, coordinates automatic refresh activity and manual intent for only that session, and becomes Ready only after validating the session and opening its local control endpoint. Its externally diagnosable states are Absent, Starting, Ready, and Faulted.
_Avoid_: legacy runtime terminology

**Startup Gate**:
The short-lived session-scoped arbitration through which every startup request passes. It either discovers a Ready Session Runtime or starts one and waits for readiness; a spawned PID or metadata record alone is not proof of ownership.
_Avoid_: PID check, process probe, runtime owner

**Session Runtime Lease**:
An exclusive file lease held for the lifetime of a Session Runtime. It is the authority that prevents overlapping owners and is released automatically when the process exits.
_Avoid_: startup lock, PID file, heartbeat

**Session Identity**:
The lossless canonical Herdr socket path that remains stable for the same default or named session across Session Stop and Session Restore. Tabby derives a SHA-256 storage key from it and embeds the original identity in persisted records so both can be validated on load.
_Avoid_: tab ID, PID, socket basename

**Transient Evaluation Failure**:
The disappearance of a tab, pane, or rename target during one bounded evaluation. It ends only that evaluation; the next trigger or periodic cycle may try again.
_Avoid_: Session Stop, runtime crash, reconnect loop

**Transient Transport Failure**:
A temporary Herdr RPC failure while the canonical session socket still exists. It ends the current evaluation but leaves the Session Runtime alive to try again on the next trigger or periodic cycle.
_Avoid_: Session Stop, terminal fault, immediate retry loop

**Terminal Runtime Fault**:
A deterministic contradiction in protocol, Session Identity, or runtime ownership. The Session Runtime fails closed and reports the fault through status rather than repeating the same operation.
_Avoid_: transient RPC failure, missing tab, Session Stop

**State Integrity Fault**:
A Terminal Runtime Fault caused by Session-Scoped Tab State that cannot be decoded or validated. Tabby preserves the original data and disables automatic renaming for that session until an explicit repair action confirms that the invalid state may be discarded.
_Avoid_: empty state, automatic reset, transient read failure

**One-Shot Refresh**:
A bounded automatic label evaluation started by a Refresh Trigger or periodic freshness cycle. A triggered evaluation includes the Focus Quiet Window; a periodic evaluation does not create one unless a trigger arrives. It takes at most three samples at a 500 ms cadence and requires two consecutive equal candidates before revalidation. A newer focus trigger invalidates the attempt. When Herdr remains responsive, all inspection and any rename complete within 2.5 seconds from the latest trigger or periodic cycle start. The evaluation applies at most one automatic label update and otherwise ends without a rename.
_Avoid_: daemon loop, background polling, continuous refresh

**Focused Pane**:
The pane within the focused tab that Herdr reports as focused. If no pane in the focused tab is reported as focused, the plugin may use the first listed pane only for Working Directory Suffix fallback.
_Avoid_: active pane, selected pane

**Label Policy**:
The validated rules used to turn process and cwd data into a Tab Label Candidate, including Significant Command allowlists, ignored commands, runner/subcommand pairs, aliases, maximum label length, and trailing Working Directory components. Version 1 starts from tested built-in defaults and may extend or present them through `config.toml`; runtime timing and safety guarantees are not part of Label Policy configuration.
_Avoid_: config, preferences, ruleset

**Process Inspector**:
The boundary that asks Herdr for foreground process details for a selected pane. If process inspection fails or returns no useful Significant Command, the plugin falls back to Working Directory Suffix rather than failing the rename loop.
_Avoid_: process_info call, ps lookup

**Herdr Session**:
A default or named Herdr context with a stable saved identity and a canonical server socket while running. Tabby's automatic behavior and persisted tab state are scoped to one Herdr Session at a time.
_Avoid_: terminal session, shell session

**Session-Scoped Tab State**:
Persistent Manually Locked Tabs, Automatic Label Baselines, and unresolved Automatic Rename Intents that belong to exactly one validated Session Identity. Tab identities are meaningful only within that session; the same `tab_id` in another session refers to unrelated state and must never inherit its state. Pending evaluations, samples, triggers, quiet windows, and deadlines do not survive Session Restore.
_Avoid_: state shared across sessions

**Forget Session Action**:
An explicit user action that removes retained Session-Scoped Tab State for one validated Session Identity. Tabby does not infer permanent deletion from Session Stop and does not expire stopped-session state automatically.
_Avoid_: stop session, automatic cleanup, unlock all

**Runtime Control Endpoint**:
A local Unix socket used by hooks to signal the Ready Session Runtime without performing refresh work themselves. It lives in a session-private `0700` directory, accepts only the owning operating-system user, validates Session Identity on every message, and never authorizes PID-based termination.
_Avoid_: public socket, refresh hook, remote API

**Cooperative Runtime Handoff**:
An authenticated replacement requested by the explicit `start` activation path, including activation after `tabby install`, when a different validated Tabby binary already owns the session. The Ready owner finishes or cancels bounded work, closes its Herdr connections and control endpoint, and releases the Session Runtime Lease before the invoking registered binary starts. A failed handoff does not authorize killing a process by PID.
_Avoid_: force restart, overlapping upgrade, PID kill

**Runtime Status**:
A read-only diagnostic view for one selected Session Identity. It reports the runtime state, version, lease ownership, last evaluation and failure, next periodic cycle, and counts of manual locks, baselines, and unresolved rename intents; reading status never starts or repairs a runtime.
_Avoid_: health repair, ensure started, mutable status
