# Desktop read-only terminal observation (D3)

> Status: implemented in source and unit-tested; macOS real-attach smoke is
> recorded below. Windows and Linux real attach, terminal input/full
> emulation, and Agent conversation are **NOT VERIFIED**.

## Product contract

The Desktop Terminal pane is a read-only observer of the terminal herdr-core
already owns:

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
- The Host remains the control/lifecycle owner. The Desktop receives no
  terminal control capability and has no code path to `terminal.kill`,
  `terminal.rm`, `terminal.interrupt`, `terminal.send` or `terminal.keys`.
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
| Attached | `terminal.attach {observe:true}` succeeded | Detach, Copy diagnostics |
| Attached · N bytes received | Visible progress counter; raw bytes, not Agent progress | Detach, Copy diagnostics |
| Closed | The core ended the stream, usually program exit with reason/exit code/signal | Retry read-only, Detach, Copy diagnostics |
| Attach failed | Local spawn, framing, capability or core-identity failure | Retry read-only, Detach, Copy diagnostics |

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

## Verification

Automated unit tests cover:

- CSI, OSC, split escape sequences, C0 controls and invalid UTF-8;
- byte/line/long-line ring bounds, partial lines and tail rendering;
- status/progress mapping, including closed-with-exit-code;
- the observe request has no input or resize flags;
- a mocked event source proves sanitization, progress and close handling;
- an ignored real-daemon test launches a test-owned terminal, observes text,
  receives a normal close, and cleans up that terminal only.

Manual macOS evidence for this change is recorded in the pull request body:
real `hc 0.1.18` framed observe attach against a test terminal. Windows and
Linux real attach were not exercised; only the shared code path and their
compile/test CI jobs apply.

## Non-goals

- No terminal input, copy-on-select, shell integration or full VT emulation.
- No terminal lifecycle control from the Desktop.
- No Agent conversation, approval or tool-call inference from terminal bytes.
- No Host RPC changes.
