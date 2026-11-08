# Desktop terminal observation and explicit one-line input (D3)

> Status: read-only observation and the explicit one-line input surface are
> implemented in source and unit-tested; macOS real observe attach is recorded
> below. Real GUI line send, Windows/Linux real attach/input, full terminal
> emulation, resize and keystroke streaming are **NOT VERIFIED**.

## Product contract

The Desktop Terminal pane has two deliberately separate paths.

### 1. Observe (default, read-only)

```text
explicit pane/detail action
  → background Desktop worker
  → hc connect (framed protocol generation 1)
  → terminal.attach {observe:true, cols:120, rows:40, history:0}
  → bounded, sanitized text in the pane
```

It never sends keystrokes, paste, named keys, resize, focus, automatic input,
interrupt, stop, kill or archive. Dropping or detaching the attachment sends
only `CLOSE` for that stream. Closing a Pane, Tab, Session or window drops the
stream; it does not signal herdr-core or the terminal program.

### 2. Explicit one-line input (opt-in, capability-gated)

Only a Session Terminal pane with an `AgentRun { core_id, terminal_address }`
binding and a live observe stream offers a composer. The terminal detail card
for an unbound Host terminal stays read-only.

```text
explicit Send click or Enter in the composer
  → HostClient::runtime_send_with_request_id
      (reads the separate agent-control token at call time)
  → Host runtime.send
      preflight: request_id, intent:"user", separate control capability,
                 one-line/size/C0/bidi text rule
      exact core_id + terminal_address check against the connected core
      audit entry for success and for every preflight denial
      in-process idempotency by request_id
  → hc terminal.send (paste this line, then Enter after the quiet period)
```

The composer is not terminal emulation:

- one line, at most 8 KiB, printable text only; newline, tab, other control
  characters and bidi overrides are rejected by both the Desktop/client and
  the Host before any backend call;
- typing is local. Nothing is sent on application load, Session selection, Tab
  switch, attach, reattach or on each keystroke;
- no resize, interrupt, stop, kill, detach, named keys or paste channel is
  reachable from this surface;
- the binding and the attached stream are re-resolved at send time. Both the
  `core_id` and the terminal address must match the live feed; a same-address
  rebind to another core, an ended stream or a stale render is refused;
- drafts are keyed by pane plus `core_id` plus terminal address, so a rebind
  cannot reuse text typed for the previous terminal;
- an unchanged draft retries with the **same** `request_id`, so a reply lost to
  a timeout is deduped by the Host instead of executing the line twice
  (within one Host process; the idempotency cache is in memory). Editing the
  draft, or a binding change, mints a new id;
- the field is cleared only after a verified Host success. A failure keeps the
  draft, shows the Host/client error verbatim and is never retried
  automatically. A Host-reported `replayed` outcome is labelled as such.

When the capability is unavailable the composer field is absent and the pane
states why: disabled with the Host's typed problem, still checking, or Host
offline. The Desktop never substitutes the workspace metadata token and never
fabricates a success state.

Raw terminal text is displayed as raw terminal text. It is **not** a
structured Agent event and is never parsed to synthesize messages, tool calls,
progress, approvals or Agent status. The Agent conversation pane remains an
explicit unsupported state until R2 supplies structured events.

## Direct hc-client read boundary

The Desktop depends on `ukk-hc-client` directly and runs
`FramedConnection::attach(AttachRequest::observe(...))` in a background worker.
This is deliberate:

- The Host line RPC (`runtime.attach`) cannot carry a backpressured byte
  stream; `crates/host/src/runtime_api.rs` already refuses it for that reason.
- The Host remains the control/lifecycle owner. The observe path receives no
  terminal control capability and has no code path to `terminal.kill`,
  `terminal.rm`, `terminal.interrupt`, `terminal.send` or `terminal.keys`.
  The input path exists only through `HostClient::runtime_send`, which needs
  the separate agent-control capability.
- The observer does not own Agent lifetime. The AgentRun binding is read-only
  metadata; the terminal process survives Desktop detach, Pane close, Session
  switch, window close and application quit.
- A core identity mismatch between the AgentRun binding (`core_id`) and the
  connected `hello.core.id` fails the attach instead of observing a different
  machine's terminal.

Security consequences:

- This is a same-user local boundary. Anyone who can read the user's profile
  or reach the herdr-core socket already has the account's shell-level access.
  The Desktop does not widen that boundary; it also does not sandbox herdr-core.
- `observe:true` is enforced locally by `ukk-hc-client`: input, automatic input
  and resize calls are refused before a frame is written. The core also does
  not treat an observer as a writer.
- Terminal output is untrusted bytes. CSI/OSC/control-string sequences are
  stripped, invalid UTF-8 is replaced lossily, and remaining C0 controls are
  removed before display. Bidi and other Unicode formatting characters are
  not interpreted as product state.
- Attach diagnostics contain addresses, core identity, status, byte counts and
  local errors only. They never include terminal contents.
- Configuration uses the same `UKK_HC_BIN` environment variable as the Host.
  A Desktop must run as the same OS user whose `hc` daemon it observes.

## Resolution and explicit attach

An attach is never started on application load, Host refresh, Session
selection or Tab switch. A user must act on one of these surfaces:

| Surface | Address source | Key |
|---|---|---|
| Session Terminal pane | `AgentRun { core_id, terminal_address }` binding | `pane:<pane-id>` |
| Agent detail terminal card | selected Host snapshot `Terminal.address` | `detail:<address>` |

The pane shows the real binding even when no stream is attached. If the
binding changes while a stream is open, the pane reports the mismatch and
offers **Reattach read-only**; the old stream is only dropped.

## States and actions

| State | Meaning | Actions |
|---|---|---|
| Connecting | `hc connect` / framed hello / attach is in flight | Detach only removes the local worker |
| Attached | `terminal.attach {observe:true}` succeeded | Detach, Copy diagnostics; one-line composer when capability is enabled |
| Attached · N bytes received | Visible progress counter; raw bytes, not Agent progress | Detach, Copy diagnostics |
| Closed | The core ended the stream, usually program exit with reason/exit code/signal | Retry read-only, Detach, Copy diagnostics; no composer |
| Attach failed | Local spawn, framing, capability or core-identity failure | Retry read-only, Detach, Copy diagnostics |
| Capability unknown | `runtime.status` has not answered yet | Recheck capability |
| Capability disabled | The separate agent-control token is absent or insecure | Recheck capability; no input field |
| Host offline | The local Host is unreachable or answered invalidly | Recheck capability; no input field |

`Closed` and `Attach failed` never imply that the Desktop stopped the program.
Only herdr-core and its owner can end the terminal.

## Bounds

Each attached terminal keeps at most:

- 256 KiB of sanitized text;
- 4096 display lines;
- 8 KiB per display line (longer lines are split);
- 400 trailing lines rendered by the pane, so the live tail stays visible.

The worker consumes stream bytes on a dedicated thread. It returns CREDIT only
for data it has consumed, so a stalled UI applies backpressure instead of
growing memory. The GPUI thread only checks a generation counter and repaints;
it never performs terminal I/O.

One submitted line is capped at 8 KiB by the Desktop composer,
`ukk-host-client` and the Host preflight before any backend call.

## Verification

Automated unit tests cover:

- CSI, OSC, split escape sequences, C0 controls and invalid UTF-8;
- byte/line/long-line ring bounds, partial lines and tail rendering;
- status/progress mapping, including closed-with-exit-code;
- the observe request has no input or resize flags;
- a mocked event source proves sanitization, progress and close handling;
- an ignored real-daemon test launches a test-owned terminal, observes text,
  receives a normal close, and cleans up that terminal only;
- one-line/C0/bidi/size validation, keyboard typing/Backspace/Escape,
  Enter-only send, capability gating (unknown/disabled/offline), draft
  retention on failure, stable request id across an unchanged-draft retry and
  a new id after an edit, clear-only-after-verified-success, core+address feed
  matching (including a same-address core rebind), draft key scoping, and that
  an attached live stream alone cannot produce input or a send request.

Host-client contract tests cover:

- the request carries the separate agent-control token, never the workspace
  token, plus `request_id`, `intent:"user"` and the exact binding;
- each new explicit line gets a fresh `request_id`; a caller-supplied id is
  reused verbatim for an unchanged-draft retry;
- Host denial, core mismatch, idempotent replay and missing/insecure/short
  capability errors are returned without retry or a fabricated success, and a
  bad capability opens no IPC connection;
- malformed lines, bindings and request ids are rejected before any IPC;
- `runtime.status` is read-only and needs no capability file.

Host runtime tests cover:

- one-line/size/C0/bidi text rejection before the backend, with a
  `denied:invalid_params` audit entry;
- a workspace-shaped `control` value rejected for `runtime.send` with a
  `denied:unauthorized` audit entry;
- preflight denials recorded through the RPC `handle_request` path.

Manual macOS evidence for the observe path is recorded in the pull request
body: real `hc 0.1.18` framed observe attach against a test terminal. Windows
and Linux real attach were not exercised; only the shared code path and their
compile/test CI jobs apply.

**NOT VERIFIED:** a real GUI line send against a real hc terminal was not run
for this change; no user terminal was mutated. Real line-send acceptance
requires a test-owned terminal and explicit evidence in a follow-up run.

## Non-goals

- No full VT emulation, keystroke streaming, resize, named-key input, paste
  channel, copy-on-select or shell integration.
- No terminal lifecycle control from the Desktop.
- No automatic input or retry that hides a failure.
- No Agent conversation, approval or tool-call inference from terminal bytes.
- No Host RPC changes in this slice.
