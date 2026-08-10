# Issue 71 Herdr 0.8 lifecycle evidence

Date: 2026-08-09  
Host contract: macOS, Herdr 0.8.0, protocol 19  
Result: passed

## Isolation

The run used `scripts/herdr_lifecycle_harness.py`. It created one temporary root under `/tmp` and redirected `HOME`, every XDG root, `TMPDIR`, and `HERDR_CONFIG_PATH` beneath it. It removed inherited Herdr socket and plugin-state variables before starting either session. Cleanup stopped both isolated servers and removed only that validated temporary root.

The harness supports `--plan --root <path>` so its full environment and default/named command topology can be inspected without starting a process. A live run writes a sanitized JSONL transcript to `.scratch/herdr-lifecycle-transcript.jsonl`; sandbox and repository paths are replaced with `<sandbox>` and `<repo>`.

## Real-runtime observations

The recorded run established these observations against the installed Herdr binary:

- A linked development manifest advertised version 0.1.10, one `[[startup]]`, the required actions, and the three focus/creation hooks.
- The default session and `tabby-lifecycle-named` ran simultaneously with distinct canonical sockets and distinct Ready Session Runtime owners.
- Sixteen concurrent startup, creation, manual, and focus ingress commands per session all returned through the same Ready owner.
- After a focus trigger, `last_evaluation_unix_ms` did not change during the first 750 ms. An evaluation followed the 1000 ms Focus Quiet Window and another began on the five-second idle cadence in each session.
- Killing the isolated named-session owner released its lease. The next `signal-created` started a different launch and returned it to Ready without an external supervisor.
- Stopping the isolated default Herdr server ended its owner. Restarting the same session invoked `[[startup]]`, created a different launch, and completed its initial evaluation after quiet.
- A headless Herdr server, with no attached client process, retained its Ready owner and periodic cadence; this is the Client Detach lifecycle shape.

Run it again with:

```sh
cargo build
python3 scripts/herdr_lifecycle_harness.py
```

## Deterministic complementary evidence

The real harness intentionally does not corrupt protocol traffic or persisted bytes inside a live operator workflow. Deterministic and multiprocess tests cover those fault paths:

| Contract | Test evidence |
| --- | --- |
| Proven stop vs timeout/no-listener | `refresh_executor` transport classification and `session_runtime` loop behavior |
| Disappearing/reused target | `one_shot_revalidation_rejects_a_reused_lifecycle_before_rename` |
| Rename target/source/default/manual reconciliation | `locks::tests::automatic_rename_intent_*` and `locks::tests::reconciliation_*` |
| Corrupt or mismatched state | `read_only_inspection_reports_identity_fault_without_repairing_it`, repair/archive tests, and status fault rendering |
| Default/named state isolation with equal tab IDs | `session_state_isolated_by_lossless_session_identity` |
| Cooperative upgrade without overlap | `cooperative_handoff_releases_the_old_process_lease_before_a_new_owner_can_acquire_it` |
| Stopped-session cleanup only | `forget_session_removes_only_an_explicitly_stopped_session_identity` and identity checks |
| No persistent subscription or inactive-tab work | manifest validation, `session.snapshot` focused observation tests, and absence of an `events.subscribe` production path |

The harness proves external lifecycle ownership and timing. Exact internal RPC counts during quiet remain a deterministic architecture assertion rather than a socket-proxy measurement; the public observation is that no evaluation timestamp advances during quiet.
