# Herdr focus and session lifecycle event contract

## Question

Which Herdr events cover mouse and keyboard focus changes across workspaces,
tabs, panes, and named sessions, and what is the smallest polling-free event
subscription Tabby can rely on?

## Scope and source baseline

This research targets the locally installed **Herdr 0.7.3** and its matching
official source tag. The annotated `v0.7.3` tag resolves to commit
`299dd4163a96381ec2d8e5bde13d7ba6d6432373`. Claims below use only Herdr's
official source, official documentation in that tag, and a sandboxed runtime
probe against the installed 0.7.3 binary.

## Resolution

There are two different answers, depending on what “polling-free” means:

1. **Functionally minimal focus subscription:** one `pane.focused` subscription
   per running Herdr session is sufficient. Every real change of Herdr's
   focused `(workspace, pane)` tuple emits `workspace.focused`, then
   `tab.focused`, then `pane.focused`; subscribing to all three would deliver
   three triggers for one focus transition. Mouse and keyboard navigation both
   use the same focus mutation path before this synchronization runs.
2. **Strictly polling-free subscription:** **the set is empty in Herdr 0.7.3.**
   Although Tabby can block on the socket without polling, Herdr implements
   every subscription connection with a dedicated loop that wakes every
   100 ms, checks connection state, polls every active subscription, and
   sleeps again. Therefore even the minimal `pane.focused` stream introduces
   recurring server-side idle work.

The strict result conflicts with a destination that requires zero polling or
recurring idle work across the whole system. The next architecture decision
must choose one of these boundaries:

- accept Herdr's upstream 10 Hz per-connection wake-up while keeping Tabby
  itself passive;
- avoid `events.subscribe` and find a Herdr-native event hook that launches
  bounded Tabby work without a persistent subscription; or
- require an upstream Herdr change from polling subscription threads to a
  blocking/notified event stream.

## Focus contract

### Available events

Herdr 0.7.3 exposes `workspace.focused`, `tab.focused`, and `pane.focused` as
subscription types. Their payloads are:

| Event | Payload |
| --- | --- |
| `workspace.focused` | `workspace_id` |
| `tab.focused` | `tab_id`, `workspace_id` |
| `pane.focused` | `pane_id`, `workspace_id` |

The event names are defined by the subscription schema and event-kind mapping;
the payload definitions confirm that `pane.focused` does not include `tab_id`.
Sources: [subscription schema](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/schema/events.rs#L11-L55),
[event names](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/schema/events.rs#L186-L240),
[focus payloads](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/schema/events.rs#L427-L477).

There is no focus-lost subscription. When `current_focus` becomes `None`,
Herdr sends a terminal focus-lost signal to the previous pane but emits no
socket focus event. This is not a gap for Tabby's rename trigger because no tab
is focused then; workspace/tab close events remain available for consumers
that need structural lifecycle state. Source:
[focus synchronization](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/api.rs#L748-L792).

### Why `pane.focused` alone covers all focus changes

`App::sync_focus_events` compares the current `(workspace index, focused pane
id)` with the previous tuple. If the tuple changed and a new focus exists, it
emits all three focus events in order: workspace, tab, pane. A pane-only focus
change therefore still emits workspace and tab events, and a workspace or tab
change still ends with `pane.focused`. Source:
[focus synchronization](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/api.rs#L748-L792).

Mouse clicks on panes and mouse actions for workspaces, tabs, and panes route
to `focus_*_via_api`. Keyboard navigation routes workspace, tab, agent/pane,
previous/next, and directional actions through the same helpers. Those helpers
call the runtime workspace/tab/pane focus APIs, after which the app loop calls
`sync_focus_events`. Sources:
[mouse dispatch](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/input/mod.rs#L255-L307),
[keyboard navigation](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/input/navigate.rs#L206-L313),
[focus API helpers](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/input/navigate.rs#L391-L486),
[app-loop synchronization](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/app/mod.rs#L879-L892).

Consequently, the smallest functionally complete request is:

```json
{
  "id": "tabby-focus",
  "method": "events.subscribe",
  "params": {
    "subscriptions": [
      { "type": "pane.focused" }
    ]
  }
}
```

Treat `pane.focused` as a **trigger**, not as the full evaluation input. Tabby
must query current focused state to recover the focused tab id and process
information.

### Subscription replay and initial evaluation

Ordinary lifecycle subscriptions initialize their cursor at sequence zero,
not at the EventHub's current sequence. The EventHub retains only its latest
512 events. A new subscription can therefore replay retained old focus events,
or receive no initial focus event if the last focus event has already fallen
out of the buffer. Sources:
[subscription initialization and polling](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/subscriptions.rs#L110-L186),
[event polling](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/subscriptions.rs#L292-L320),
[512-event retention](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/event_hub.rs#L1-L45).

The safe consumer contract is therefore:

1. connect and wait for `subscription_started`;
2. immediately run one bounded evaluation of current focus;
3. use later `pane.focused` records only to trigger another bounded evaluation;
4. coalesce redundant/stale triggers where useful.

## Session and socket lifecycle contract

Herdr uses newline-delimited JSON over a Unix-domain socket, and an event
subscription keeps that connection open after its acknowledgement. The default
session and every named session have different sockets:

```text
~/.config/herdr/herdr.sock
~/.config/herdr/sessions/<name>/herdr.sock
```

Sources: [socket transport and paths](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/website/src/content/docs/socket-api.mdx#L489-L516),
[session path construction](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/session.rs#L157-L180).

There is no `session.started`, `session.stopped`, `server.started`, or
`server.stopped` event kind in 0.7.3. `herdr session list` is a point-in-time
filesystem scan whose `running` flag is computed by checking that the socket
exists and accepting a connection; it is not a cross-session stream. Sources:
[complete event-kind enum](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/schema/events.rs#L186-L212),
[session enumeration](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/session.rs#L187-L225),
[running check](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/session.rs#L388-L390).

For an already-connected session, server shutdown sets the API server's
`running` flag false, removes its owned socket path, and causes subscription
handlers to end; the subscriber observes EOF/disconnect. Sources:
[server-handle shutdown](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L31-L53),
[connection stop condition](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L542-L550).

Thus Herdr subscriptions can signal that **their own connected session socket
ended**, but no session can announce another session's creation or startup.
Polling-free discovery of new sessions requires an out-of-band per-session
startup mechanism or an OS filesystem watcher plus socket verification; the
latter is not a Herdr API guarantee.

## The upstream idle-polling constraint

Herdr's docs describe later event lines as pushed, but the 0.7.3 server
implementation services each open subscription with a polling thread. The
constant is 100 ms, and the loop checks connection state, iterates all active
subscriptions, then sleeps for that interval. Sources:
[poll interval](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L23-L29),
[subscription loop](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L453-L508),
[documented stream behavior](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/website/src/content/docs/socket-api.mdx#L622-L656).

This is an upstream implementation detail, not network/API polling performed
by Tabby, but it is recurring idle CPU wake-up work attributable to every
subscriber. One subscriber per active session therefore means one additional
Herdr subscription thread waking about ten times per second per session.

## Runtime verification

The live probe used a temporary `HOME`, `XDG_CONFIG_HOME`,
`HERDR_CONFIG_PATH`, and `HERDR_SOCKET_PATH`; it did not write to the operator's
real Herdr configuration. It started the installed Herdr 0.7.3 server,
subscribed **only** to `pane.focused`, and issued raw JSON-RPC mutations over
separate socket connections.

Observed transcript:

```text
ACK {"id":"sub","result":{"type":"subscription_started"}}
workspace.create focus=true  -> pane_focused w1:p1, workspace w1
tab.create focus=true        -> pane_focused w1:p2, workspace w1
pane.split focus=true        -> pane_focused w1:p3, workspace w1
workspace.create focus=true  -> pane_focused w2:p1, workspace w2
workspace.focus w1           -> pane_focused w1:p3, workspace w1
server.stop                  -> subscriber EOF
socket exists after stop     -> false
```

This confirms with the shipped binary that `pane.focused` alone observes
workspace-, tab-, and pane-level focus changes and that shutdown produces EOF
and socket removal.

### Commands run

```sh
herdr --version
herdr status --json
brew info herdr --json=v2
git ls-remote https://github.com/ogulcancelik/herdr.git \
  refs/tags/v0.7.3 'refs/tags/v0.7.3^{}'
curl -fsSL \
  https://github.com/ogulcancelik/herdr/archive/refs/tags/v0.7.3.tar.gz \
  -o /tmp/herdr-v0.7.3.tar.gz
tar -xzf /tmp/herdr-v0.7.3.tar.gz --strip-components=1 \
  -C /tmp/herdr-v0.7.3
python3 /tmp/herdr_focus_contract_probe.py
```

Two focused upstream unit tests were also attempted:

```sh
cd /tmp/herdr-v0.7.3
cargo test \
  clicking_unfocused_pane_with_mouse_reporting_focuses_it_via_left_button \
  -- --exact --nocapture
cargo test \
  terminal_direct_focus_pane_shortcut_switches_focus_without_leaving_terminal_mode \
  -- --exact --nocapture
```

They could not build because the environment lacks `zig`, which Herdr's build
script requires for vendored `libghostty-vt`:

```text
failed to execute zig build for vendored libghostty-vt:
No such file or directory
```

The installed-binary socket probe passed; mouse/keyboard equivalence is
established from the tagged primary source paths above rather than from those
unbuilt tests.

## Remaining uncertainties

- Herdr's event schema does not promise forever that one tuple change will emit
  all three focus events. `pane.focused`-only is verified for 0.7.3 and should
  be treated as a versioned compatibility contract.
- EOF proves that the connection ended, not why. Tabby must distinguish
  expected shutdown, transient socket failure, and a replaced/restarted server
  by reconnecting and validating socket/session identity.
- A cross-platform filesystem watch could discover socket creation without
  periodic polling, but Herdr does not document socket-directory notifications
  as a public API. It cannot be considered a portable Herdr contract without a
  separate design/prototype decision.
