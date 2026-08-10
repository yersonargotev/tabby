# Validate the issue #47 architecture against Herdr 0.8.0

Date: 2026-08-09  
Status: completed research; ADR 0010 records the resulting decision.

## Question

Does the Event-Driven Session Runtime specified in [issue #47](https://github.com/yersonargotev/tabby/issues/47) remain the best design for session-isolated automatic labels now that the installed Herdr version is 0.8.0?

The investigation compared the local binary (`herdr --version` returned `0.8.0`) with the official [`v0.8.0` release](https://github.com/herdrdev/herdr/releases/tag/v0.8.0) at commit [`346411fa21afd297f5ed3b3fa56f9e3fbf7654b7`](https://github.com/herdrdev/herdr/tree/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7). Claims about Tabby come from this repository and its closed planning issues. No secondary sources were used.

## Conclusion

The earlier map retains valuable local guarantees: one owner per session, a lifetime lease, a serialized Startup Gate, bounded evaluation, local control, session-isolated state, and crash-safe rename intent. Its external ingress is no longer the best fit for Herdr 0.8.0.

Tabby can receive lifecycle and focus signals through native plugin hooks instead of a persistent `events.subscribe` connection:

```text
[[startup]]                 -> tabby ensure-started
[[events]] on=pane.focused  -> tabby signal-focus
tabby signal-focus          -> Session Runtime control endpoint
```

Herdr runs `[[startup]]` for normal server startup and restored/handoff startup, and runs matching `[[events]]` commands with the emitting session's environment. See the [plugin startup and environment contract](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/docs/preview/website/src/content/docs/plugins.mdx#L193-L264), [startup hook dispatch](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api/plugins/runtime.rs#L183-L216), [event hook dispatch](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api/plugins/runtime.rs#L218-L266), and [allowed event names](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/events.rs#L286-L309).

This hook-based ingress preserves one serialized Tabby owner and focus-triggered behavior while deleting Herdr subscription negotiation, stream recovery, and subscription-attributable upstream polling.

The research initially treated zero recurring idle work as a possible product requirement. The later Q1-Q23 product review rejected that requirement: foreground activity may change without focus, and Herdr exposes no event for every such change. ADR 0010 therefore retains hook-based ingress but intentionally runs one bounded focused evaluation every five seconds while the Session Runtime is Ready.

## Verified Herdr 0.8 behavior

### `pane.focused` covers focus transitions

Herdr 0.8.0 retains `pane.focused` as both a subscription and an event. See the [subscription type](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/events.rs#L17-L61) and [event type](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/events.rs#L194-L251).

When focus changes, Herdr emits `workspace.focused`, `tab.focused`, and `pane.focused` in order. A hook on `pane.focused` therefore observes workspace, tab, and pane focus transitions without three duplicate ingress paths. See [focus synchronization](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api.rs#L814-L860).

The event carries identity, not a transactional rename condition. Tabby must still read current state before mutation. Herdr 0.8 exposes no focus-generation-conditional rename operation in its [request schema](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/request.rs).

### `[[startup]]` covers new and restored servers

Herdr loads enabled plugins and runs their `[[startup]]` commands after normal headless server creation and after server handoff/restore. See [normal startup](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/server/headless.rs#L4707-L4737) and [handoff startup](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/server/headless.rs#L4819-L4840).

This removes the restored-session coverage gap that existed in Herdr 0.7.x. It does not provide Tabby ownership: Herdr launches plugin commands asynchronously, so concurrent hooks must still cross Tabby's serialized Startup Gate. See [command execution and concurrency handling](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api/plugins/runtime.rs#L80-L179).

### Persistent subscriptions poll upstream

After acknowledging `subscription_started`, Herdr checks each active subscription and sleeps for 100 ms in a loop. See the [subscription server loop](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/server.rs#L646-L702) and its [100 ms constant](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/server.rs#L29).

Event subscriptions scan events retained in a 512-entry buffer rather than blocking on a notification primitive. See [subscription polling](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/subscriptions.rs#L332-L340) and [EventHub storage](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/event_hub.rs#L1-L43).

A persistent Tabby subscription therefore adds approximately ten Herdr wakeups per second while idle. Hook ingress avoids this cost. Tabby's accepted five-second evaluation cadence is separate, deliberate product work used to observe fixed-focus foreground changes.

### `session.snapshot` improves observation coherence

Herdr 0.8.0 exposes `session.snapshot`, which returns focus plus all workspaces, tabs, and panes in one response. See the [snapshot schema](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/session.rs#L9-L25) and [server construction](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/app/api/session.rs#L16-L56).

The snapshot can replace separate `tab.list` and `pane.list` calls for each sample and revalidation. It does not include foreground processes, so Tabby still needs `pane.process_info`; see the [process fields](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/api/schema/panes.rs#L441-L465). The sequence snapshot → process info → rename remains non-atomic.

## Alternatives

| Alternative | Result |
| --- | --- |
| `[[startup]]` + `pane.focused` hooks into one local runtime | Recommended. Covers restore and focus without a persistent Herdr subscription. |
| Event hooks that run refresh directly | Rejected. Concurrent short-lived evaluators cannot provide latest-trigger-wins or one writer. |
| Startup hook plus one-shot refresh only | Rejected for the product objective. It does not observe later focus or fixed-focus foreground changes. |
| One `events.subscribe` client per session | Rejected. It duplicates hook capability and retains the 100 ms upstream loop. |
| Global filesystem/socket supervisor | Rejected. It adds cross-session discovery and ownership outside Herdr's plugin model. |

## Architecture implications

- Use Session Runtime terminology, not Session Subscriber or Hybrid Session Refresher.
- Keep the Startup Gate, lifetime lease, serialized owner, local control, session identity, and Automatic Rename Intent concepts.
- Use `[[startup]]` for new/restored ownership and `pane.focused` for primary focus ingress. Creation/manual hooks may recover a missing owner.
- Ensure signal commands never perform RPC, sleeps, or rename work.
- Use `session.snapshot` plus `pane.process_info` behind Focused Observation.
- Remove Herdr 0.7 adapters, strict/compatibility modes, subscription negotiation, and event-stream recovery.
- Preserve the moderate five-second periodic evaluation because Continuous Focused-Tab Freshness is an accepted product requirement.

## Limits

The official source establishes implementation behavior for the exact Herdr 0.8.0 release, not a permanent compatibility promise for future releases. A later Herdr upgrade must revalidate the plugin schema, startup/restore dispatch, focus hook behavior, snapshot schema, protocol version, and any proposed subscription implementation before Tabby changes this architecture.
