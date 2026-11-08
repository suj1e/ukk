# omp RPC structured Agent integration

> Status: **source implemented; deterministic mock tests and strict clippy pass
> locally; real omp 18.18.1 smoke passed on macOS. Three-OS CI on the PR is the
> authority for merge readiness.** · protocol v1 · 2026-11-08
>
> This slice adds the first real structured Agent provider to ukk. It does not
> add approvals, a tool-approval UI, WAN/relay, host tools, or persistence of
> the omp transcript.

## What exists

| Layer | Source | Contract |
| --- | --- | --- |
| Transport client | new `crates/omp-client` | Typed newline-delimited JSON client for `omp --mode rpc --no-session` |
| Host service | `crates/host/src/{omp_service,omp_timeline}.rs` | Host-owned process, bounded timeline, long-poll snapshots, agent-control mutations |
| Host RPC | `crates/host/src/rpc.rs` | `omp.start` / `omp.prompt` / `omp.abort` / `omp.stop` / `omp.snapshot` / `omp.wait` |
| Desktop client | `crates/host-client` | Typed `omp_*` calls plus the separate agent-control token |
| Desktop surface | `apps/desktop/src/workspace/omp.rs` | Session action + conversation panel with a real composer |

## Process ownership rules

1. The **Host owns the child**. The Desktop never spawns, signals or waits for
   an omp process.
2. Opening or closing the omp panel, closing a window, switching Sessions, or
   dropping a `HostClient` connection has **no Host-side effect**. There is no
   per-view teardown in `omp_service`.
3. Only `omp.abort` (which cancels the active turn, not the process),
   capability-gated `omp.stop` (explicit kill after a wedged turn), or Host
   shutdown stops the child.
4. Host shutdown closes stdin and waits, then kills and reaps. On Unix a
   dedicated SIGTERM/SIGINT handler calls `omp_service::shutdown_all()` and
   exits, so systemd/launchd/task stop paths execute the same cleanup. On
   Windows the service stop path does not run that handler; the child exits on
   stdin EOF when the Host process closes the pipe. Both behaviours were
   checked manually on macOS; automated signal-stop coverage is not present.
5. **Host restart does not reattach** to a running omp session in this slice.
   The snapshot carries an explicit `restart_note`. The omp session id could be
   captured for a future `--resume`, but that is not implemented.

## Capability separation

- `omp.snapshot` and `omp.wait` are **read-only** and require no capability.
  They return the bounded conversation snapshot and never change process state.
- `omp.start`, `omp.prompt`, `omp.abort` and `omp.stop` require the existing default-off
  **agent-control** token (`AgentControlAuth`), plus `request_id` and
  `intent: "user"`. The workspace metadata token and the plugin-control token
  are both rejected by construction.
- Every mutation is audited through the existing agent-control audit log with
  `core_id: "omp"` and the Session id as the terminal field. Tokens never appear
  in audit entries, snapshots or errors.
- Missing or insecure (symlink/non-0600/short) token files are reported as a
  visible error; there is no silent fallback or implicit escalation.
- `request_id` is an idempotency key for `omp.prompt`/`omp.abort`/`omp.stop`.
  A repeated completed id returns the current snapshot without executing again
  (up to 128 ids are retained); a duplicate still in flight returns the current
  snapshot. An explicit `success:false` rejection releases the id so it can be
  retried; a timeout/transport failure keeps it reserved because omp may have
  accepted the command. The prompt gate atomically transitions Idle/Complete →
  Running under one lock, so two concurrent prompts cannot both write.

Enable control explicitly:

```sh
# path used by default: <host socket>.agent-control on macOS/Linux,
# %LOCALAPPDATA%\ukk\run\agent-host.agent-control on Windows
printf '%s:%s\n' "$(uuidgen)" "$(uuidgen)" > ~/.ukk/run/agent-host.agent-control
chmod 600 ~/.ukk/run/agent-host.agent-control
```

The Desktop reads the same file at the moment of an explicit Start/Prompt/Abort
click and never stores the token.

## Wire contract (omp RPC v1)

The client spawns a configurable binary (`omp` by default) with separate
stdin/stdout/stderr pipes and the default arguments `--mode rpc --no-session`.
The Host reads `UKK_OMP_BINARY` and `UKK_OMP_ARGS` (whitespace-split) for
integration testing and non-standard installations; the Desktop never supplies
a binary. It waits for `ready`, validates:

- `protocolVersion == 1`
- `supportedProtocolVersions` contains 1 (when present)
- `maxFrameBytes >= 1024`, clamped to an 8 MiB absolute ceiling

The child's `maxFrameBytes` becomes the inbound **and** outbound line cap
(bounded further by the caller's cap). Protocol v2 is never negotiated.

### Commands implemented

| Command | Semantics |
| --- | --- |
| `prompt { message }` | Correlated by `id`; acknowledgement means accepted, not finished |
| `steer { message }` | Typed; acknowledged |
| `follow_up { message }` | Typed; acknowledged |
| `abort` | Typed; completion still requires a terminal `agent_end` |

The Host refuses a second `omp.prompt` while a turn is running (`-32041`)
instead of silently steering. The client exposes typed `steer`/`follow_up` for
a future queue UI.

### Completion

A turn is complete only when:

- `agent_end.isTerminal != false`, or
- `prompt_result.agentInvoked == false` / `response.data.agentInvoked == false`
  (a local command such as a slash command ran no agent).

`isTerminal: false` keeps the turn open and is rendered as a real status entry.
Events may arrive before the acknowledgement; responses are matched by `id`,
never by arrival order.

### Frames handled without deadlock

- `extension_ui_request`: fire-and-forget methods (`setWidget`, `notify`,
  `setStatus`, `setTitle`, `set_editor_text`, `cancel`, `open_url`) are recorded
  and never answered. Value-expecting methods (`select`, `confirm`, `input`,
  `editor`) are answered `{ "cancelled": true }`, so an extension cannot
  deadlock the Host. Real omp 18.18.1 emits `setWidget` headlessly.
- `host_tool_call`: answered with a `host_tool_result` error (this slice
  registers no host tools).
- `host_uri_request`: answered with an `isError` result (no URI schemes).
- `rpc_chunk`: reported as an unsupported v2 frame; the client never negotiated
  chunking, so it will not guess at reassembly.
- Unknown frame types are counted (`unknown_frames`) and ignored. Malformed
  lines and oversized frames are reported as typed notices and the stream
  continues. A line longer than the cap is drained without buffering.

### Frames → timeline

Only documented fields are rendered:

| omp frame | Timeline entry |
| --- | --- |
| `message_start`/`message_update`/`message_end` (`text_delta`) | one streaming `assistant` entry |
| `message_update` (`thinking_delta`) | `thinking` entry |
| `tool_execution_start`/`update`/`end` | one `tool` entry; partial output replaces the previous snapshot, result text and `isError` on end |
| `message_start`/`end` with role `toolResult` | not duplicated (tool events cover it) |
| `notice`, `error`, `extension_error` | `notice`/`error` entry, redacted |
| `prompt_result` (local), `command_output` | `status` entry |
| `agent_start`/`turn_start`/`turn_end`/`agent_end` | turn state only |
| unsupported/malformed/oversized frames | truthful notice entries |

No approval is synthesized, no tool name is inferred from terminal text, and
there is no terminal rendering in this surface.

### Bounds

- Timeline: 400 entries, 8 KiB per entry, 512 KiB total across all retained
  entries. The oldest entries are discarded and `truncated: true` is reported;
  a serialized snapshot therefore stays well below the Host's 4 MiB reply cap.
- Prompt input: 64 KiB (Host), 8 KiB (Desktop composer).
- stderr: 64 redacted lines, 4 KiB per line, drained on its own thread and never
  merged into the JSON parser.
- Long-poll: 1–10 s per call, generation-based. A timeout returns the current
  snapshot; it never mutates.

### Snapshot shape

```json
{
  "session_id": "…",
  "cwd": "/project",
  "state": "running",
  "protocol_version": 1,
  "provenance": "omp RPC · local",
  "generation": 12,
  "cursor": 41,
  "turn": {"state": "complete", "agent_invoked": true,
           "started_ms": 1, "finished_ms": 2},
  "timeline": [
    {"seq": 1, "at_ms": 1, "kind": "user", "text": "Reply with exactly: OMP-OK",
     "is_error": false, "truncated": false},
    {"seq": 2, "at_ms": 2, "kind": "assistant", "text": "OMP-OK",
     "is_error": false, "truncated": false}
  ],
  "truncated": false,
  "unknown_frames": 0,
  "stderr_tail": [],
  "exit": null,
  "error": null,
  "restart_note": "Host restart does not reattach to a running omp process; start a new session"
}
```

`omp.wait` takes `session_id`, `after_generation` and `timeout_ms`, and returns
the same shape. `cursor` is the last omp event sequence folded in.

## Host RPC surface

| Method | Capability | Notes |
| --- | --- | --- |
| `omp.snapshot` | none | read-only |
| `omp.wait` | none | read-only long-poll; dropping the caller changes nothing |
| `omp.start` | agent-control | idempotent per Session while running; max 4 children |
| `omp.prompt` | agent-control | one active turn at a time |
| `omp.abort` | agent-control | cancels the turn; the child stays alive |
| `omp.stop` | agent-control | kills and reaps the child after a wedged turn; the Session record remains and `omp.start` can replace it |

Error codes: `-32030` unauthorized/disabled, `-32034` unknown session,
`-32040` session limit, `-32041` turn busy/no turn, `-32042` omp failure,
`-32043` not running, `-32044` omp timeout, `-32602` invalid params.

## Desktop surface

The Session detail has an **omp RPC** action. Its panel renders:

- `omp RPC · local` provenance, protocol version, generation, state, cwd;
- streaming user/assistant/thinking/tool/notice/error/status entries;
- an explicit **Start omp session** action (never automatic);
- a composer (typing, Backspace, Enter) plus **Send** and **Abort**;
- Host errors verbatim (sanitized/redacted), no retry loop for mutations.

The panel polls through `HostClient::omp_wait` on the GPUI background executor.
View state is local UI state: no SQLite layout/schema migration is made.
Closing the panel increments a local poll token and cancels the long-poll task;
it never calls a Host mutation.

Composer limitation: this GPUI app has no shared text-input entity yet, so the
composer handles typed characters, Backspace and Enter only. IME composition,
selection, paste and cursor movement are **NOT VERIFIED / not implemented**;
the Host API accepts any prompt text sent by a future input widget.

## Tests and evidence

Deterministic mock child (bin target of `ukk-omp-client`, used only by tests):

```sh
cargo test -p ukk-omp-client           # 15 integration + 10 unit tests
cargo build -p ukk-omp-client --bins   # builds ukk-omp-mock-rpc
cargo test --workspace                 # Host tests locate the mock in target/debug
cargo test --manifest-path apps/desktop/Cargo.toml --bin ukk-desktop
```

Mock scenarios cover: ready handshake, text streaming, tool events, local
`agentInvoked:false`, command rejection, no response/timeout, malformed JSON,
oversized frames, exit before/after acknowledgement, stderr separation, out-of-
order responses, delayed acknowledgement, `extension_ui_request` confirmation
(answered `cancelled`), typed steer/follow_up, and graceful shutdown.

Host tests cover: capability denial (wrong/missing/disabled tokens), workspace
token rejection, read-only snapshot/wait, start/prompt/abort, bounded timeline,
no child stop on view close, unknown session, idempotent start, busy-turn
rejection, and audit entries.

### Real omp smoke (macOS)

```sh
cargo test -p ukk-omp-client --test real_omp -- --ignored --nocapture
```

Actual captured frames from omp 18.18.1 (abridged):

```text
ready: protocol=1 supported=[1, 2] maxFrame=1048576 maxReassembled=67108864
prompt ack: PromptAck { agent_invoked: None, data: None }
event[1]: ExtensionUi { id: "…", method: "setWidget", requires_response: false }
event[3]: AgentStart
event[5]: MessageStart(MessageEvent { role: "user", text: Some("Reply with exactly: OMP-OK"), .. })
event[10]: MessageUpdate { delta_kind: "text_delta", delta: "O", .. }
event[11]: MessageUpdate { delta_kind: "text_delta", delta: "MP-OK", .. }
event[13]: MessageEnd(MessageEvent { role: "assistant", text: Some("OMP-OK"), .. })
event[16]: AgentEnd { is_terminal: true }
assistant text: "OMP-OK"
```

```text
event[38]: MessageEnd(MessageEvent { role: "assistant", text: None, .. })
event[39]: ToolExecutionStart(ToolEvent { tool_name: Some("bash"),
           text: Some("{\"command\":\"printf OMP-TOOL-OK\"}"), .. })
event[40]: ToolExecutionUpdate(ToolEvent { text: Some("OMP-TOOL-OK"), .. })
event[43]: ToolExecutionEnd(ToolEvent { text: Some("OMP-TOOL-OK"), is_error: Some(false) })
event[44]: MessageStart(MessageEvent { role: "toolResult", text: Some("OMP-TOOL-OK"), .. })
event[51]: MessageUpdate { delta_kind: "text_delta", delta: "TO", .. }
event[56]: MessageEnd(MessageEvent { role: "assistant", text: Some("TOOL-DONE"), .. })
event[59]: AgentEnd { is_terminal: true }
```

Both smoke tests shut the child down; the tests use an ephemeral
`--no-session` child in a temporary directory and never write session files or
credentials into the repository.

### Real local socket end-to-end (macOS)

A real `ukk-host serve` was started with a temporary `UKK_SOCKET`, a
provisioned agent-control token file (0600) and a temporary audit path, then
driven over the actual Unix socket JSON-RPC with real omp 18.18.1:

```text
PING {"capability":"separate-agent-control","enabled":true,
      "host_restart_reattaches":false,"protocol_version":1,
      "read_only":["omp.snapshot","omp.wait"],"view_close_stops_omp":false, ...}
SNAPSHOT_UNKNOWN {'code': -32034, 'message': 'unknown omp session'}
UNAUTHORIZED_CODE -32030                 # omp.start with a wrong control token
START_STATE running gen 1
PROMPT_TURN running
FINAL state=running gen=8 cursor=18 turn=complete error=None
TL status "omp RPC session started · protocol v1 · omp RPC · local"
TL user "Reply with exactly: OMP-OK"
TL notice "omp `setWidget` UI update ignored (headless Host)"
TL assistant "OMP-OK"
ASSISTANT_TEXT "OMP-OK"
STATE_AFTER_IDLE running                 # repeated snapshots did not stop it
ABORT_IDLE_CODE -32041                   # no turn was active
WAIT_GEN 8
```

`ps` showed the child `omp --mode rpc --no-session` parented by the Host; after
stopping the Host, both the Host and the omp child were gone (stdin EOF / Host
shutdown), with no orphan. The audit file contained `omp.start` denial and
success, `omp.prompt` success, and no token material.

## NOT VERIFIED

- Real GUI interaction with the omp panel (click/type/abort) on macOS, Windows
  and Linux. Desktop CI is source/unit-test only in this branch; the local
  macOS build in this session used a cached GPUI target because the machine's
  MetalToolchain component is missing.
- Windows Named Pipe path for `omp.*` and real omp runs on Windows/Linux.
- Automated SIGTERM/SIGINT shutdown test. The Unix handler and macOS manual
  stop were exercised; Windows service-stop cleanup relies on stdin EOF and is
  not verified.
- Host restart reattachment / `--resume` of an omp session.
- Protocol v2 chunk negotiation and reassembly (oversized frames are rejected
  honestly instead).
- omp approvals/tool-approval UI, host tools, host URI schemes and `rpc-ui`.
- IME composition, paste and selection in the composer.
- Model/thinking selection from the Desktop (available in omp's protocol, not
  exposed in this slice).
- Long-running subagents (`set_subagent_subscription` frames are not requested;
  only the top-level session stream is rendered).
