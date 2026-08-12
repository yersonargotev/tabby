# Issue 88: agent-derived title contract

Status: completed research. This is a **no-go** for an automatic agent-derived
Tab Label Candidate in the current Tabby/Herdr contract. It makes no product or
architecture commitment, so `CONTEXT.md` and the ADR set are intentionally
unchanged.

## Question

Can Tabby obtain a stable, authoritative, opt-in agent-derived Tab Label
Candidate for Codex and Claude from Herdr 0.8.0 / protocol 19, rather than
confusing session identity, terminal presentation, and display metadata?

## Decision

**No-go: do not implement this source now.** The existing bounded observation
path (`session.snapshot`) carries all three fields, but it does not turn any of
them into a proven task-title contract:

- `agent_session` is an optional identity/resume reference, not a title. It was
  absent for both real managed sessions in this run, including after safe
  context requests.
- `terminal_title` is OSC-derived terminal presentation. It changed while
  Codex was working and is reset by the shell; it has no agent-task semantics,
  no length contract, and no authoritative update event devoted to titles.
- `title` is display metadata from a reporting source. It reliably carried the
  controlled values in this run, including equal, empty, and rapid writes, but
  source sequencing, guards, TTL, and last-report ordering make it a
  presentation channel—not an automatically generated Codex/Claude task
  title.

Tabby must retain the established Significant Command / Working Directory
Suffix Label Policy. A future implementation requires an upstream or agent
integration contract that explicitly publishes a bounded, source-owned,
monotonic task-title field and its clear/restore rules. If that exists, name
the feature after that new field, not “agent title.”

## Primary-source baseline

The installed binary reported `herdr 0.8.0`; `session.snapshot` reported
protocol 19. The official annotated release tag resolves to
[`346411fa`](https://github.com/herdrdev/herdr/commit/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7).
All source links below pin that release.

Herdr's published `PaneInfo` schema has separate optional `agent_session`,
`terminal_title`, `terminal_title_stripped`, and `title` fields; none is a
required task-title field. [`AgentSessionInfo` is a four-part
reference](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/agents.rs#L151-L158),
and [`PaneInfo` exposes the fields independently](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/panes.rs#L313-L352).

| Field | Herdr-owned semantics | Classification | Suitability as current automatic candidate |
| --- | --- | --- | --- |
| `agent_session` | Agent-report session reference, represented as source, agent, id-or-path kind, and value. [`pane.report_agent_session`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api/panes.rs#L1238-L1266) builds that reference. | Identity | No: it identifies a resumable agent context when supplied; it says nothing about the task. |
| `terminal_title` | Latest raw terminal title observed from terminal output; `terminal_title_stripped` only removes a known leading activity glyph. [`terminal title synchronization`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/terminal_titles.rs#L28-L67), [`stripping rule`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/terminal/title.rs#L1-L28). | Transient terminal presentation | No: OSC writers, shells, and clients can change it independently of the agent's task. |
| `title` | Effective presentation metadata selected from accepted reports; newest valid report wins, optional TTL expiry hides it, and agent/source guards can reject it. [`report schema`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/panes.rs#L291-L311), [`acceptance and mutation`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/terminal/metadata.rs#L144-L288), [`selection and expiry`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/terminal/metadata.rs#L395-L516). | Metadata presentation | No: it is an extensible display channel, not a Codex/Claude-owned task contract. |

## Reproducible observation

[`scripts/herdr_agent_title_contract_harness.py`](../../scripts/herdr_agent_title_contract_harness.py)
creates isolated Herdr/XDG state and an agent home beneath `/tmp`, copies only
Codex's authentication file when present, and deletes that temporary root
afterward. Claude uses its normal platform authentication. Codex runs read-only;
Claude runs in safe mode with no tools; both start in an empty temporary
workspace. The explicit execution flag acknowledges that real authenticated
model requests may incur cost. The harness never reads terminal output; it
redacts prompt text, local paths, and command output. Uncontrolled terminal
titles receive stable numbered tokens, preserving equality and change without
publishing their contents. Apart from those declared field-level redactions,
the JSONL retains the raw API event envelopes and the candidate-field snapshot
projection. Its minimally redacted raw wire trace is
[`docs/evidence/issue-88-agent-title-contract.jsonl`](../evidence/issue-88-agent-title-contract.jsonl).

Run:

```sh
python3 scripts/herdr_agent_title_contract_harness.py \
  --run-authenticated-agents \
  --output docs/evidence/issue-88-agent-title-contract.jsonl
```

The run has schema version 1, contains no harness failure/event-stream-error
records, discovers both `codex` and `claude`, and includes snapshots and API
events. The following check is the artifact gate:

```sh
jq -s '
  all(.[]; .schema_version == 1) and
  all(.[]; .kind != "failure" and .kind != "event-stream-error") and
  ([.[] | select(.kind == "snapshot") | .panes[]? |
    select(.agent == "codex" or .agent == "claude") | .agent] | unique) ==
    ["claude", "codex"]
' docs/evidence/issue-88-agent-title-contract.jsonl
```

### Results matrix

“Observed” means the redacted snapshot/event trace proves the behavior; it
does not infer task content from terminal text.

| Required situation | Codex | Claude | Result and consequence |
| --- | --- | --- | --- |
| Real managed agent start | Observed | Observed | `agent` detection and `pane.agent_detected` events were observed for both. Neither emitted `agent_session`. |
| Two distinct safe context requests | Both delivered | Both delivered | The socket acknowledged distinct literal-text and Enter input for each real agent pane; snapshots followed every delivery. Terminal content and model completion are deliberately outside the retained evidence. No task-title field appeared. |
| Empty, repeated, and rapid presentation values | Observed through `title` | Observed through `title` | Controlled metadata records retained repeated and rapid values and cleared on `clear-title`. This proves metadata transport only. |
| OSC terminal-title attempts | Observed as transient/reset behavior | Observed as transient/reset behavior | Fixed OSC values were not stable at snapshot because shell/client title writers superseded them; empty clear was observed. This rejects terminal-title stability. |
| Pane/tab focus changes | Observed | Observed | Focus events and coherent snapshots switched panes without producing a task-title field. |
| Client attach then detach | Observed | Observed | Both panes remained observable after the real client detached; no title semantics changed. |
| Session Stop and Session Restore | Observed | Observed | A fresh server on the same isolated persistence roots restored the pane layout, but contained no live agents, identities, or presentation fields. Herdr has no separate restore command: loading persisted state on server start is its documented restore path. |
| Additional cold restart | Observed | Observed | Same result: no live pane/agent title survives server stop. |

The raw event trace includes `pane.updated`, agent detection, and focus events.
That is useful bounded transport for a *future* titled source, but it does not
deliver a dedicated title event or prove that every metadata/OSC change is
event-delivered. Herdr's generic event subscriptions start from sequence zero
and replay the event hub; consumers must observe the current snapshot rather
than treat an event payload as authoritative state. [`ActiveEventSubscription`
initialization and polling](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/subscriptions.rs#L94-L210),
[`event replay cursor`](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/subscriptions.rs#L332-L340).

## Bounded observation versus new ingress

`session.snapshot` is sufficient to *read* these fields coherently; Tabby
already uses it for focused observation. It is not sufficient to derive a new
candidate because no current field carries the required semantics. Therefore:

- **No new ingress is justified for any current field.** Polling/adding a
  subscription cannot convert identity or presentation into authoritative task
  data.
- **A viable future source needs new ingress at its producer boundary:** an
  official Codex/Claude integration (or upstream Herdr contract) must send a
  source-scoped task-title update with a monotonic sequence and explicit
  clear/replacement event. Once Herdr includes it in `PaneInfo` and emits an
  update that covers it, Tabby's existing snapshot-plus-bounded-trigger shape
  can consume it.

## Required contract before reconsidering

The minimum upstream/integration contract must specify all of the following:

1. A dedicated name (for example `agent_task_title`), producer ownership, and
   an opt-in capability for Codex and Claude.
2. A source and monotonic sequence; equal values are valid idempotent updates;
   older values cannot overwrite newer ones.
3. Empty versus absent semantics: empty explicitly clears; absent means no
   update. It must state whether a last non-empty value can be shown while a
   new task starts (recommended: no).
4. A bounded, documented truncation unit and privacy policy supplied by the
   producer, rather than Tabby guessing from OSC text.
5. An event whose payload/revision covers the new field, plus snapshot
   reconciliation after subscribe/reconnect.
6. Stop/restore behavior: live task title is cleared on Session Stop; it may
   reappear only after the restarted agent republishes it. Tabby's Manually
   Locked Tab still wins over any automatic candidate.

Until then, manual locks must remain the final precedence rule and no stale
metadata, stale terminal title, or agent session identifier may be renamed
into a tab label.

## Objective limitations

- The local agents were real installed Codex CLI 0.147.0 and Claude Code
  2.1.220. The trace proves request delivery but deliberately does not retain
  prompts, responses, or make completion timing part of the contract.
- Agent start did not publish an `agent_session` in either case. This is a
  meaningful negative observation, not proof that a separately configured
  integration can never report one.
- A headless shell can reset an OSC title immediately after a controlled
  writer exits. That demonstrates transience; it does not measure every
  terminal emulator or future agent version.
- Session Restore here is Herdr's documented loading of persisted layout state
  when a new server starts against the same roots after `server stop`; it is
  not a claim that an exited terminal process or agent session resumes. See
  [Session State](https://herdr.dev/docs/session-state/).
