# Herdr protocol compatibility policy

Date: 2026-08-19

## Question

Can Tabby safely accept every Herdr protocol greater than 18, instead of adding an explicit Herdr release/protocol pair whenever Herdr updates?

## Conclusion

Not on the basis of the protocol number alone.

Herdr's `PROTOCOL_VERSION` identifies the binary client/server wire format, while Tabby uses Herdr's separate newline-delimited JSON socket API. The change from Herdr 0.8.0/protocol 19 to 0.8.2/protocol 20 changed the binary wire format but did not change the three JSON API contracts Tabby consumes. This makes an exact release/protocol allowlist unnecessarily coupled to an adjacent protocol.

However, Herdr does not explicitly guarantee that every future protocol greater than 18 will preserve all earlier JSON methods and response fields. Its documentation calls the CLI/socket API stable, but only says protocol changes are reviewed for release compatibility and directs clients to check the protocol before depending on new behavior. Therefore, a bare `protocol >= 19` rule would be an inference rather than an upstream compatibility guarantee.

The adopted policy is to validate the JSON contract Tabby actually consumes, while retaining Herdr 0.8.0 and protocol 19 as minimum sanity baselines. This preserves fail-closed startup without maintaining an exact version/protocol allowlist.

## Documented facts

### Herdr's protocol number belongs to the binary wire protocol

Herdr defines `PROTOCOL_VERSION` in `src/protocol/wire.rs`. Its source comment says that the value is bumped when the wire format changes incompatibly. Herdr 0.8.0 defines protocol 19, while Herdr 0.8.2 defines protocol 20.

Sources:

- [Herdr 0.8.0 binary wire protocol, protocol 19](https://github.com/herdrdev/herdr/blob/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7/src/protocol/wire.rs#L15-L16)
- [Herdr 0.8.2 binary wire protocol, protocol 20](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/src/protocol/wire.rs#L15-L16)

The Herdr CLI itself requires exact equality between its protocol and the running server's protocol before sending normal API requests. This is a client/server binary compatibility policy; it does not establish a compatible numeric range.

Source: [Herdr CLI protocol compatibility guard](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/src/cli.rs#L777-L798)

### Tabby uses the JSON socket API, not the binary wire format

Tabby opens the selected Unix socket, writes one JSON request followed by a newline, and reads one response line. Its runtime uses exactly these methods:

- `session.snapshot`
- `pane.process_info`
- `tab.rename`

Sources:

- [Tabby's newline-delimited socket transport](https://github.com/yersonargotev/tabby/blob/84e2da44a7c564a4080710bd802580b7b0ece209/src/herdr_client.rs#L93-L110)
- [Tabby's JSON method calls](https://github.com/yersonargotev/tabby/blob/84e2da44a7c564a4080710bd802580b7b0ece209/src/herdr_client.rs#L135-L189)

Herdr documents this API as newline-delimited JSON over a Unix domain socket on Unix and a named pipe on Windows. It also exposes the complete request, response, error, event, and subscription schema through `herdr api schema --json`.

Sources:

- [Herdr socket transport](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/socket-api.mdx#L647-L664)
- [Herdr installed JSON Schema](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/socket-api.mdx#L20-L34)

### Exact difference between protocol 19 and protocol 20

The protocol bump was introduced when Herdr added `ServerMessage::TerminalBell` to the binary wire protocol. Herdr 0.8.2 also contains binary client/server additions for direct graphics, pixel mouse input, and related acknowledgements, including `AppDirectGraphics`, `GraphicsTransmissionResult`, `InputPixels`, `MouseCapture.sgr_pixels`, and `GraphicsFile`.

Sources:

- [Commit that bumps the protocol for terminal-bell forwarding](https://github.com/herdrdev/herdr/commit/6f311498aeeb27c0973781961ef94e8d0016ed17)
- [Herdr 0.8.0 to 0.8.2 comparison](https://github.com/herdrdev/herdr/compare/346411fa21afd297f5ed3b3fa56f9e3fbf7654b7...9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c)

None of these additions belongs to the JSON methods used by Tabby.

The JSON definitions for `SessionSnapshot` and `TabRenameParams` are unchanged between the 0.8.0 and 0.8.2 tags. `src/api/schema/panes.rs` gained graphics and input definitions, but the `PaneProcessInfo` definition used by Tabby did not change.

Relevant 0.8.2 definitions:

- [`SessionSnapshot`](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/src/api/schema/session.rs#L8-L23)
- [`TabRenameParams`](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/src/api/schema/tabs.rs#L27-L31)
- [`PaneProcessInfo`](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/src/api/schema/panes.rs#L489-L514)

Consequently, Herdr 0.8.0/protocol 19 and 0.8.2/protocol 20 are compatible with Tabby because the JSON subset Tabby consumes is equivalent, not because protocol 20 is generally backward-compatible with protocol 19.

### Upstream compatibility guarantees are limited

Herdr describes its CLI/socket API as stable in its plugin documentation.

Source: [Herdr plugin API stability statement](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/plugins.mdx#L13-L16)

Herdr's plugin manifest models compatibility as a lower bound: authors set
`min_herdr_version` to the oldest release that supports the APIs, events, and
manifest fields they use. The manifest has no corresponding maximum-version
field. This supports forward-compatible plugin evolution, but it still does not
define an unconditional compatibility range for every raw JSON method.

Source: [Herdr `min_herdr_version` guidance](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/plugins.mdx#L99-L104)

Its protocol stability section is less specific. It says protocol changes are reviewed for release compatibility, advises checking the server protocol before depending on new behavior, and tells clients to handle unknown fields gracefully. It does not define a backward-compatible protocol range, a semantic-versioning policy for individual JSON methods, or capability negotiation for the methods Tabby uses.

Source: [Herdr protocol stability guidance](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/socket-api.mdx#L929-L934)

No explicit upstream guarantee was found that all future `protocol > 18` releases will retain `session.snapshot`, `pane.process_info`, and `tab.rename` with the fields and semantics required by Tabby.

### Latest Herdr release at the research cutoff

The latest stable release available on 2026-08-19 was Herdr 0.8.2, published at 2026-08-19 18:00:03 UTC. Its annotated tag resolves to commit `9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c`.

Source: [Herdr v0.8.2 release](https://github.com/herdrdev/herdr/releases/tag/v0.8.2)

A later preview build existed on the same date, but it was a prerelease rather than a newer stable release.

## Inference and adopted policy

The protocol number is an unreliable proxy for Tabby's compatibility because it can change for binary terminal-client features that Tabby never uses. The release version is also an indirect proxy: two releases may expose the same required JSON subset even when their binary protocol differs.

Tabby defines and validates a required JSON API subset:

1. Keep `min_herdr_version = "0.8.0"` as the installation and feature baseline.
2. Before the runtime becomes Ready, run `api schema --json` through the
   `HERDR_BIN_PATH` injected by the plugin host, then require compatible request
   and response shapes for `session.snapshot`, `pane.process_info`, and
   `tab.rename`.
3. Perform read-only live probes for `session.snapshot` and `pane.process_info` against the exact selected session socket.
4. Validate all fields Tabby semantically requires. Continue ignoring additional fields, as Herdr recommends.
5. Validate the `tab.rename` request and response shape from the schema rather than probing it at startup, because a live probe would mutate user-visible state.
6. Fail closed with a diagnostic naming the missing or incompatible method or field.

Using `HERDR_BIN_PATH` avoids accidentally inspecting another Herdr executable
from `PATH`; Herdr documents it as the running binary provided to plugin
commands.

Source: [Herdr plugin runtime environment](https://github.com/herdrdev/herdr/blob/9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c/docs/preview/website/src/content/docs/plugins.mdx#L251-L281)

Under this policy, the numeric protocol can remain diagnostic information and may be required to be at least 19, but it should not be the primary compatibility gate. A `protocol >= 19` check without schema and behavior validation is not recommended.

Schema validation cannot prove that future releases preserve every behavior behind an unchanged shape. The project should therefore retain the lifecycle harness as release evidence, but adding a new Herdr release would no longer require a code change solely because an unrelated binary protocol number changed.

For a stronger and simpler long-term contract, Tabby can ask Herdr upstream to expose one of the following:

- a separately versioned JSON socket API;
- an explicit compatibility range for that API; or
- capabilities for the individual methods and response fields Tabby requires.

There is no primary-source basis for treating every protocol greater than 18 as automatically safe. Tabby's dynamic validation is therefore the authoritative runtime policy; the `0.8.0 / 19` and `0.8.2 / 20` runs remain an evidence matrix rather than an allowlist.
