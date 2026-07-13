# Current Hybrid Session Refresher baseline

## Question

What CPU, memory, wake-up, RPC, latency, and failure characteristics does the
current Hybrid Session Refresher exhibit per active Herdr session, including
transient socket errors and tab-close races?

## Resolution

The current refresher is inexpensive in absolute process terms, but it is not
idle or resilient:

- one release-mode Tabby process used **2.08 MiB RSS** (**1.14 MiB physical
  footprint**) and **0.00059% average CPU** during one 31.1-second measured
  idle window;
- after the initial one-second quiet window it performs a full focused-tab
  evaluation every five seconds: normally `tab.list`, `pane.list`, and
  `pane.process_info`, plus `tab.rename` when required;
- the observed steady cadence was seven evaluations / 21 read RPCs in 31.1
  seconds, or about **0.675 read RPC/s per active session**;
- the persistent event connection also causes Herdr 0.7.3's subscription
  thread to wake every 100 ms. A controlled 15-second window measured about
  **9.5 extra interrupt wake-ups/s** in Herdr with one idle subscriber, close
  to the source-defined 10 Hz loop. This cost belongs to Herdr, not the Tabby
  process;
- a new candidate was first inspected about one second after subscription,
  but was not renamed until the next five-second poll: **6.11 seconds after
  subscription acknowledgement** in the cold-start trace;
- an RPC connection loss, event-stream EOF, server shutdown, or stale
  `tab.rename` target terminates the refresher with exit code 1. There is no
  reconnect or retry loop;
- closing the focused tab between `tab.list` and `pane.list` was benign in the
  tested interleaving, but closing it immediately before `tab.rename` killed
  the refresher with `tab_not_found`.

These numbers make the replacement comparison concrete: it should remove the
five-second evaluation cadence (21 read RPCs in this sample), avoid Tabby's
recurring read timeout wake-ups, and recover rather than exit on all four
tested connection/lifecycle failures. Whether Herdr's upstream 10 Hz
subscription wake-up is acceptable remains a separate architecture boundary.

## Source-derived behavior

This baseline targets Tabby commit
[`392c62a`](https://github.com/yersonargotev/tabby/tree/392c62a69c820b56daa9f54432b08dc65ffb8db1)
and Herdr 0.7.3.

### Timing and RPC sequence

The constants are a 1,000 ms focus quiet window and a 5-second idle poll
interval. Startup acts like a focus event, so the first evaluation is scheduled
after one second; subsequent evaluations are scheduled five seconds after the
previous evaluation begins. Every recognized focus/create event resets the
quiet window and discards any pending rename. Sources:
[`src/daemon.rs`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L28-L30),
[`HybridRefresherState`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L131-L175),
and the
[`run_hybrid_refresher_loop`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L813-L843).

A normal evaluation calls `tab.list`, then `pane.list`, then
`pane.process_info` for the selected focused pane. It optionally calls
`tab.rename` after label stability is reached. Sources:
[`tick`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L251-L407)
and the
[`HerdrApi` implementation](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/herdr_client.rs#L255-L289).

The transport opens a fresh Unix socket for every RPC; only the event
subscription connection persists. Thus a normal idle evaluation creates three
short-lived socket connections in addition to the long-lived subscription.
Sources:
[`UnixSocketTransport`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/herdr_client.rs#L66-L106)
and
[`HerdrEventStream`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/herdr_client.rs#L138-L214).

### The apparent 400 ms revalidation path is unreachable

`HybridRefresherState.pending_rename` is initialized to `None`, cleared on
focus/create events, read by `hybrid_tick_and_save_locks`, and cleared by
revalidation. No production code assigns `Some(...)`. Consequently the
`revalidate_pending_rename` branch is unreachable and the 400 ms
`DEFAULT_REFRESH_STABILIZATION_DELAY` does not reduce Hybrid Refresher label
latency. The next observation comes from the five-second poll. This conclusion
was verified both by searching every assignment and by the proxy trace: first
candidate inspection at 1.109 seconds after subscription acknowledgement,
then `tab.rename` 5.003 seconds later.

### Failure propagation

`tab.list`, `pane.list`, and `tab.rename` use `?`, so their errors leave the
tick and then the infinite loop. Event-stream errors also use `?`. Only
`pane.process_info` is explicitly caught and converted into an optional
diagnostic so label selection can fall back to pane data. Sources:
[`tick`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L259-L290),
[`rename path`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L347-L390),
and
[`event loop`](https://github.com/yersonargotev/tabby/blob/392c62a69c820b56daa9f54432b08dc65ffb8db1/src/daemon.rs#L828-L843).

The Herdr-side subscription cost is distinct. Herdr 0.7.3 services every open
subscription connection with a loop whose fixed interval is 100 ms; it checks
connection state and active subscriptions, then sleeps. Sources:
[`CONNECTION_POLL_INTERVAL`](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L23-L29)
and the
[`subscription loop`](https://github.com/ogulcancelik/herdr/blob/299dd4163a96381ec2d8e5bde13d7ba6d6432373/src/api/server.rs#L453-L508).

## Runtime method

### Environment

| Component | Value |
| --- | --- |
| Host | macOS 26.5.2, Apple Silicon (`arm64`) |
| Tabby | 0.1.9 source at `392c62a`, `cargo build --release` |
| Herdr | installed 0.7.3 |
| Rust | rustc/cargo 1.93.0 |

Every scenario created a new temporary directory and set all of:

```text
HOME=/tmp/tabby-baseline-*/home
XDG_CONFIG_HOME=/tmp/tabby-baseline-*/xdg
XDG_STATE_HOME=/tmp/tabby-baseline-*/state
HERDR_CONFIG_PATH=/tmp/tabby-baseline-*/config.toml
HERDR_SOCKET_PATH=/tmp/tabby-baseline-*/real.sock
TABBY_LOCK_STORE_PATH=/tmp/tabby-baseline-*/locks.json
```

No operator Herdr or Tabby configuration was read or written. Each scenario
started `/opt/homebrew/bin/herdr server` on the temporary socket, created one
workspace with two tabs, and stopped the temporary server afterward.

### Instrumentation

The release binary connected through a temporary Python Unix-socket proxy.
The proxy preserved newline-delimited JSON and persistent subscription
streaming while recording each request method, monotonic timestamp, and
upstream round-trip latency. Controlled failure cases either closed one proxy
connection or performed `tab.close` against the real socket immediately before
forwarding a selected request.

Process resource counters came from macOS `proc_pid_rusage` with
`RUSAGE_INFO_V4`, sampled once per second. `ri_user_time`, `ri_system_time`,
`ri_resident_size`, `ri_phys_footprint`, `ri_pkg_idle_wkups`, and
`ri_interrupt_wkups` are cumulative kernel counters; deltas below exclude the
first sample. The field contract is the installed macOS SDK's primary header,
`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/sys/resource.h`.

Commands:

```sh
cargo build --release
rustc --version
cargo --version
herdr --version
sw_vers
uname -m

# The harness creates all temporary paths, launches the server/proxy/refresher,
# samples proc_pid_rusage, injects failures, prints JSON, and removes the dirs.
python3 /tmp/tabby_baseline_probe.py \
  | tee /tmp/tabby-baseline-results.json

git diff --check
```

## Idle resource and RPC results

Sample size: one 32.12-second run, with a 31.11-second resource delta (32
one-second samples), seven complete evaluation cycles, and 23 measured RPC
round trips including the subscription acknowledgement and rename.

### Tabby process

| Metric | Result |
| --- | ---: |
| User CPU | 0.0535 ms |
| System CPU | 0.1292 ms |
| Average CPU | 0.000587% |
| RSS at end | 2,179,072 bytes (2.08 MiB) |
| Physical footprint at end | 1,196,320 bytes (1.14 MiB) |
| Package-idle wake-ups | 1 |
| Interrupt wake-ups | 6 |

The six interrupt wake-ups over six five-second gaps are consistent with the
Tabby process blocking in a socket read with a deadline, not spinning. They do
not include Herdr's subscription thread.

### RPC count and cadence

| Method | Count | Request offsets from process start (s) |
| --- | ---: | --- |
| `events.subscribe` | 1 | 0.286 |
| `tab.list` | 7 | 1.394, 6.395, 11.396, 16.396, 21.397, 26.398, 31.399 |
| `pane.list` | 7 | 1.394, 6.395, 11.397, 16.397, 21.398, 26.399, 31.400 |
| `pane.process_info` | 7 | 1.395, 6.397, 11.398, 16.398, 21.400, 26.400, 31.401 |
| `tab.rename` | 1 | 6.398 |

Each evaluation began 5.000-5.001 seconds after the previous one. The single
rename occurred on the second observation, not on a 400 ms deferred path.

### Local socket round-trip latency

These figures include proxy scheduling and Herdr handling, so they are an
end-to-end local baseline rather than pure Tabby execution time.

| Method | n | min | median | p95 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tab.list` | 7 | 0.292 ms | 0.387 ms | 0.463 ms | 0.499 ms |
| `pane.list` | 7 | 0.535 ms | 1.224 ms | 1.273 ms | 1.310 ms |
| `pane.process_info` | 7 | 0.241 ms | 0.503 ms | 0.528 ms | 0.528 ms |
| `tab.rename` | 1 | 1.669 ms | 1.669 ms | 1.669 ms | 1.669 ms |

## Herdr subscription-side wake-ups

One Herdr process was measured in three consecutive 15-second windows: no
subscriber, one raw idle `pane.focused` subscriber, then no subscriber after
closing it. The first window included server warm-up, so the post-subscription
window is the better control for marginal idle cost.

| Window | Package-idle wake-ups | Interrupt wake-ups | CPU |
| --- | ---: | ---: | ---: |
| No subscriber, before (warm-up contaminated) | 62 | 119 | 0.02169% |
| One idle subscriber | 100 | 263 | 0.00171% |
| No subscriber, after | 37 | 120 | 0.00082% |

Relative to the post-subscription control, one idle subscription added 63
package-idle and 143 interrupt wake-ups in 15 seconds: **4.2** and **9.53 per
second**, respectively. Kernel wake-up categories are not one-to-one with Rust
`sleep` calls, but the interrupt delta agrees closely with the source-defined
10 Hz subscription loop. The extra Herdr RSS observed while subscribed was
16 KiB of physical footprint and about 400 KiB of resident pages; a single
short run is insufficient to treat those page-level differences as stable.

## Failure and tab-close race results

Each injected scenario used one fresh server/refresher pair (`n = 1` per
interleaving).

| Scenario | Result | Time / evidence |
| --- | --- | --- |
| Drop second `tab.list` response | refresher exited 1 | 6.13 s; `Herdr closed the socket without a response` |
| Close event stream after acknowledgement | refresher exited 1 | 2.01 s; `Herdr closed the event subscription` |
| Stop Herdr server | refresher exited 1 | 2.13 s; event subscription EOF |
| Close focused tab immediately before forwarding `tab.rename` | refresher exited 1 | 6.25 s; `tab_not_found: tab w1:t2 not found` |
| Close focused tab between `tab.list` and `pane.list` | refresher remained alive | alive after 8 s; later focus event/evaluations continued |

The benign race does not prove all list/process interleavings are safe. It
shows only that if the pane disappears before `pane.list`, `select_pane_for_tab`
can skip the stale tab and the subsequent Herdr focus event resets evaluation.
The rename race demonstrates the unsafe terminal edge: a stale target error is
not classified as an expected tab lifecycle outcome and disables automatic
renaming for the remainder of that Herdr session.

`pane.process_info` failure was not separately injected in the runtime harness;
its non-fatal behavior is explicit and covered by the direct `match` in
`tick`. All other failure claims above were exercised against the release
binary.

## Limitations

- Resource measurements are one short run on one Apple Silicon host, not a
  benchmark distribution. Sub-millisecond CPU totals and page-level memory
  differences should be treated as order-of-magnitude baselines.
- The Python proxy adds latency and threads outside both measured processes.
  It does not alter request count or the five-second schedule.
- Herdr's first no-subscriber window includes startup work. The after window
  is the cleaner control, but alternating many A/B windows would give a tighter
  wake-up estimate.
- There was one trial per injected race. The proxy makes each tested ordering
  deterministic, but it does not enumerate every possible tab-close point.
- The workspace used two tabs and one focused pane. Additional panes or tabs
  change response payload sizes, while the current algorithm still makes one
  `tab.list`, one `pane.list`, and at most one `pane.process_info` per tick.
- macOS `proc_pid_rusage` wake-up categories are kernel accounting metrics, not
  a direct count of source-level timers. Source inspection establishes which
  timer owns each recurring behavior.

## Comparison baseline for the replacement

Per active, idle session, compare the replacement against:

1. **Tabby process:** 2.08 MiB RSS, 1.14 MiB footprint, approximately zero CPU,
   but one interrupt wake-up every five seconds.
2. **Recurring Tabby work:** three read RPCs every five seconds for a focused
   inspectable pane (0.6 RPC/s nominal; 0.675/s in the finite 31.1-second
   sample because it includes both endpoints).
3. **Subscription ownership:** one persistent event socket; Herdr, not Tabby,
   adds an approximately 10 Hz subscription service loop.
4. **Fresh-label latency:** roughly one-second quieting plus up to the next
   five-second stability observation; 6.11 seconds after acknowledgement in
   the cold-start sample.
5. **Recovery:** none. Tested RPC EOF, event EOF, server shutdown, and stale
   rename all terminated the process.
