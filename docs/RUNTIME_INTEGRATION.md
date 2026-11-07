# herdr-core runtime integration (R1)

> Status: implemented and mock-verified; real-daemon evidence recorded below.
> R1's Host runtime service/RPC surface is a follow-up PR on the same branch
> series. This document pins the upstream contract it must target.

`crates/hc-client` talks to `hc connect`. It implements protocol generation 1
from the actual upstream schema; it does **not** infer methods, shapes or
framed behaviour from terminal output.

## Pinned upstream

| Item | Value |
|---|---|
| Repository | <https://github.com/herdrdev/herdr-core> |
| Release tag | `v0.1.18` |
| Commit | `3965a46caae095a3ead01de237fa13955e52b8c6` |
| OpenRPC version | `0.1.18`, generation `1` |
| License | Apache-2.0 |
| `schema/hc.openrpc.json` sha256 at tag | `60942ea9cba377e6732051d156b37fd5dc10b4d6b936a33d17437f229b9776ca` |
| `hc schema` from installed 0.1.18 sha256 | `769dd0cf6ddf39200d53a15f6eaccf1f2c6469f30c2ce25e3cc6f08dfbe6004b` |
| `schema/README.md` last changed at tag | `f2a3688f1da05f847ff83bec2c74236ea902c33d` |

The two schema hashes differ only because `hc schema` pretty-prints; parsed
JSON is deeply equal at v0.1.18. `schema/released.openrpc.json` is byte-identical
to `schema/hc.openrpc.json` at the tag.

Master was inspected at `9ce385d9c8abb041dd4437476d770365b3fb5282`
(2026-11-07, commit *revert: drop the core's own-hc fallback*). At that time the
OpenRPC JSON was semantically identical to v0.1.18. `schema/README.md` can carry
unreleased wording between releases; **behaviour is pinned to v0.1.18**, the
binary that produced the recorded acceptance evidence. A future release must be
re-pinned deliberately, not inferred.

The release archive used for CI acceptance is pinned by
`scripts/install_hc.py` (`herdr-core-macos-aarch64.tar.gz` sha256
`77d8af092e25ae650f4424d6dad27514f6916a11d807fb0752858cd27c21e4fc`).

## Methods implemented by the typed client

| Method | Typed request | Typed result | Notes |
|---|---|---|---|
| `hello` | `{framed:true, generation:1}` | `Hello {generation, version, core, framed, methods, features}` | Capability document for every connection. |
| `terminal.list` | `all`, `labels` | `ListResult {terminals, hidden_finished, machines}` | Read-only. |
| `terminal.get` | `terminal` | `Terminal` | Used for binding verification and idempotent launch. |
| `terminal.run` | `RunRequest` | `RunResult` | Launch/run; name is unique among live terminals. |
| `terminal.attach` | `AttachRequest` | `AttachResult` + `Attachment` | Framed connection required. |
| `terminal.send` | `SendRequest` | `SendResult` | Paste + Enter after the quiet period. |
| `terminal.keys` | `KeysRequest` | `KeysResult` | Named keys. |
| `terminal.interrupt` | `InterruptRequest` | `InterruptResult` | Non-escalating stop key. |
| `terminal.kill` | `terminal` | `Terminal` | The documented program end; product "stop". |
| `terminal.rm` | `terminal` | `RemoveResult` | Finished record only; runtime half of archive. |
| `terminal.keep` | `terminal`, `keep` | `Terminal` | Loss/restart policy, not a stop. |
| `terminal.read` | `ReadRequest` | `ReadResult` | Text snapshot. |
| `server.status` | `{}` | `ServerStatus` | Core identity/version/holders. |
| `server.stop` / `server.restart` | `{}` | `StopResult` | Exposed but never used by view lifecycle. |
| `events.subscribe` | `EventFilter` | `SubscribeResult` + `EventSubscription` | Framed channel with `seq`. |

`rpc.discover` remains available through the legacy raw call for diagnostics.
The typed API refuses any method absent from `hello.methods` with
`HcError::Unsupported { method, reason }`; there is no speculative fallback.

## Framed protocol

Every frame is:

```text
[channel u32 LE][kind u8][len u32 LE][payload]
```

Payload maximum 16 MiB (`MAX_FRAME_PAYLOAD`). A header that declares more is
rejected **before** allocation and the connection is closed, because frame
boundaries cannot be resynchronised.

| kind | name | direction | payload |
|---|---|---|---|
| 0 | DATA | both | channel 0 JSON-RPC; other channels raw stream bytes |
| 1 | CREDIT | viewer → core | u32 LE consumed bytes |
| 2 | CLOSE | both | empty (viewer detach) or JSON reason (core) |
| 3 | RESIZE | viewer ↔ observer | u16 LE cols, u16 LE rows |
| 4 | FOCUS | viewer → core | 1 = gained, 0 = lost |
| 5 | FACTS | viewer → core | `Display` JSON |
| 6 | AUTOMATIC_INPUT | viewer → core | terminal-generated reply bytes |

Unknown kinds are decoded as `Frame::Unknown` and ignored, matching the
documented rule that old cores ignore unknown local kinds.

### Attach lifecycle

1. The connection performs `hello {framed:true, generation:1}` in plain mode
   and switches after the reply.
2. `terminal.attach` returns `{channel, terminal, automatic_input,
   resize_notifications}`. The reply precedes stream frames.
3. The core sends the first paint, then live bytes. The viewer sends DATA for
   keys/paste; RESIZE/FOCUS/FACTS as they change.
4. A stream starts with 256 KiB credit. The viewer returns CREDIT for consumed
   bytes. A viewer more than 4 MiB behind is repainted instead of receiving an
   unbounded backlog.
5. The viewer detaches by sending CLOSE; the core answers with CLOSE. When the
   program ends, CLOSE carries `{reason, exit_code, signal}`.

`observe` attachments never type, resize or send AUTOMATIC_INPUT; the client
enforces that locally as well. AUTOMATIC_INPUT requires both the core's
`automatic_input` feature and `automatic_input: true` in the attach reply;
otherwise the client refuses instead of falling back silently.

### Client bounds

* request payloads and frames: 16 MiB;
* per-attachment local queue: 1 MiB / 4096 frames, overflow returns
  `HcError::Backpressure` and discards the backlog;
* orphan frames that arrive between an attach reply and channel registration:
  64 frames / 256 KiB;
* one-shot Host calls: bounded by `HcClient::call_timeout` and always
  kill/reap the child on write failure, read failure or timeout.

`Attachment::read_chunk` credits exactly the bytes it returns.
`Attachment::next_event` does not credit until the caller chooses to, so a slow
consumer exerts backpressure instead of growing memory.

## Sequence gaps

Only `events.subscribe` has sequence numbers. An attach stream is an ordered
byte stream with **no per-frame sequence** in generation 1; this client does
not invent one. Gap detection therefore applies to event notifications:

* an unfiltered subscription must see `seq = last + 1`; a jump returns
  `HcError::SequenceGap { expected, got }`;
* a filtered subscription legitimately skips numbers because other terminals'
  events still advance `seq`, so it only requires monotonic progress;
* `missed {seq, terminals}` is authoritative: the cursor resets to `seq` and
  the caller must replace its snapshot;
* reconnect establishes a new subscription/cursor. `seq` restarts with the
  core, so a lower snapshot after reconnect is not a gap.

## Idempotency

The upstream API has no idempotency-key field. The client adds only guarantees
the API supports:

* `terminal_run_idempotent` requires an explicit unique terminal `name`. It
  looks the name up first; a live terminal is reused only when argv and cwd
  prove the same request. A same-named but different live terminal is
  `HcError::IdempotencyConflict`, never a second spawn. A `name_taken` race is
  re-checked the same way.
* `terminal_send_once` dedupes by a caller key **in memory on one connection
  only**. It is never persisted and never replayed after reconnect because
  re-sending user input is unsafe.
* JSON-RPC request ids are allocated per connection; duplicate/late responses
  are ignored rather than confused with a new request.

## Lifecycle distinctions

| Product concept | Runtime call | Effect |
|---|---|---|
| Close a view / detach | frame CLOSE (or drop the connection) | Ends this stream only. The program keeps running. |
| Interrupt | `terminal.interrupt` | Presses esc (status-reporting programs) or ctrl+c. Never escalates. |
| Stop | `terminal.kill` | Ends the program: hangup → terminate → kill. There is no softer stop method in generation 1. |
| Archive metadata | ukk SQLite only | Never touches herdr-core. |
| Remove finished record | `terminal.rm` | Only a finished terminal; a live one returns `live`. |

Closing a Pane/Tab/Session/window or quitting the Host therefore has no call
path to `terminal.kill`. The Host runtime service (follow-up PR) makes this
explicit as a typed view-close action that performs zero runtime calls.

## Known-unsupported / deliberately absent

* No server-side idempotency keys; see above.
* No sequence number on attach frames; gap detection is event-stream only.
* No "pause", "soft stop" or "restart program" method in generation 1.
* No permission or approval model inside herdr-core: anyone who can reach the
  socket can run commands. ukk's separate agent-control capability is the
  Host-side gate, not an upstream feature.
* `terminal.attach` needs framed mode; plain mode returns `needs_framed`.

## Real-daemon acceptance evidence

Environment: macOS arm64, hc 0.1.18 installed at `~/.local/bin/hc` and
checksum-verified from `https://releases.herdr.dev/core/0.1.18/manifest.json`.
`hc schema` produced the sha256 listed above.

Runbook:

```sh
UKK_HC_BIN=/path/to/hc cargo test -p ukk-hc-client \
  --test real_daemon -- --ignored --nocapture
```

The test creates one uniquely named terminal, receives its first paint,
detaches, closes the client connection, reconnects and reattaches, then
interrupts, explicitly stops and removes only that terminal. Fill in observed
results here after each run; CI remains mock-based.

Observed on 2026-11-08 (macOS arm64, hc 0.1.18, client code at `73f791b`):

* `real_core_reports_the_expected_schema_identity` passed: generation 1,
  version 0.1.18, and every required method/feature advertised.
* `real_core_launch_attach_reconnect_interrupt_stop_remove` passed: launched
  one uniquely named `sleep 30` terminal, received the first paint, detached,
  closed the client connection, reconnected and reattached with a fresh
  paint, subscribed to events, interrupted, explicitly stopped (state
  `finished`) and removed only that terminal.

Result: **2 passed** in 0.10 s. PR #25 checks (test + real-hc acceptance) were
green on macOS, Ubuntu and Windows at `73f791b`; this documentation edit is a
later doc-only commit. Any environment where the binary is unavailable is NOT
VERIFIED; use the runbook above and record the version/schema hash.
